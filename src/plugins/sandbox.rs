//! A plugin's Lua state: only the safe standard libraries, a `require` that
//! sees nothing but the plugin's own files, a memory cap and an instruction
//! hook that enforces time budgets and cancellation.

use super::permissions::PermissionSet;
use super::{CallContext, PluginCode};
use mlua::{
    HookTriggers, Lua, LuaOptions, LuaSerdeExt as _, StdLib, Table, Value, VmState,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Memory one Lua state may use.
const MEMORY_LIMIT: usize = 64 * 1024 * 1024;
/// Time Lua code may run for bar, inspector, form and event handlers. Time
/// spent waiting in host functions (a process, a file read) doesn't count.
pub const QUICK_BUDGET: Duration = Duration::from_secs(2);

/// The time budget of the call in progress.
#[derive(Debug, Default)]
pub struct Budget {
    deadline: Cell<Option<Instant>>,
}

impl Budget {
    pub fn start(&self, budget: Option<Duration>) {
        self.deadline.set(budget.map(|budget| Instant::now() + budget));
    }

    /// Runs `f` (which waits on something outside Lua) without spending the
    /// budget.
    pub fn paused<R>(&self, f: impl FnOnce() -> R) -> R {
        let started = Instant::now();
        let result = f();
        if let Some(deadline) = self.deadline.get() {
            self.deadline.set(Some(deadline + started.elapsed()));
        }
        result
    }

    /// How long a process may run when the plugin sets no timeout: 30
    /// seconds inside a budgeted call (a bar handler), unlimited in jobs.
    pub fn default_timeout(&self) -> Option<Duration> {
        self.deadline.get().map(|_| Duration::from_secs(30))
    }
}

/// Reporting from a job back to its row in the jobs list.
#[derive(Debug)]
pub struct JobLink {
    pub id: u64,
    /// Progress of the item being run maps to `base + fraction * scale`.
    pub base: Cell<f32>,
    pub scale: Cell<f32>,
    pub label: RefCell<String>,
    pub last_report: Cell<Option<Instant>>,
}

impl JobLink {
    pub const fn new(id: u64) -> Self {
        Self {
            id,
            base: Cell::new(0.0),
            scale: Cell::new(1.0),
            label: RefCell::new(String::new()),
            last_report: Cell::new(None),
        }
    }

    /// Sends progress, at most ten times a second unless `force`.
    pub fn report(&self, fraction: f32, label: Option<String>, force: bool) {
        if let Some(label) = label {
            *self.label.borrow_mut() = label;
        }
        let now = Instant::now();
        if !force
            && self
                .last_report
                .get()
                .is_some_and(|last| now - last < Duration::from_millis(100))
        {
            return;
        }
        self.last_report.set(Some(now));
        super::send(super::PluginMsg::Job {
            id: self.id,
            event: super::JobEvent::Progress {
                fraction: fraction
                    .clamp(0.0, 1.0)
                    .mul_add(self.scale.get(), self.base.get())
                    .clamp(0.0, 1.0),
                label: self.label.borrow().clone(),
            },
        });
    }
}

/// What host functions need: who is calling and what it may do.
pub struct Services {
    pub code: Arc<PluginCode>,
    pub permissions: PermissionSet,
    pub cancel: Arc<AtomicBool>,
    pub budget: Rc<Budget>,
    pub job: Option<JobLink>,
}

/// A loaded plugin: its Lua state and the table `main.lua` returned.
pub struct Vm {
    pub lua: Lua,
    pub module: Table,
    pub budget: Rc<Budget>,
    pub cancel: Arc<AtomicBool>,
}

/// Lua module names map to files: `lib.args` is `lib/args.lua`.
fn module_file(name: &str) -> Option<String> {
    let valid = !name.is_empty()
        && name
            .split('.')
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
    valid.then(|| format!("{}.lua", name.replace('.', "/")))
}

/// Creates the plugin's Lua state and runs `main.lua`.
pub fn create(
    code: &Arc<PluginCode>,
    permissions: PermissionSet,
    cancel: Arc<AtomicBool>,
    job: Option<JobLink>,
) -> Result<Vm, String> {
    let lua = Lua::new_with(
        StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8 | StdLib::COROUTINE,
        LuaOptions::default(),
    )
    .map_err(|error| error.to_string())?;
    lua.set_memory_limit(MEMORY_LIMIT)
        .map_err(|error| error.to_string())?;
    let budget = Rc::new(Budget::default());
    {
        let budget = Rc::clone(&budget);
        let cancel = Arc::clone(&cancel);
        lua.set_hook(
            HookTriggers::new().every_nth_instruction(10_000),
            move |_, _| {
                if cancel.load(Ordering::Acquire) {
                    return Err(mlua::Error::runtime("cancelled"));
                }
                if budget
                    .deadline
                    .get()
                    .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    return Err(mlua::Error::runtime(
                        "took too long (plugin code may run for 2 seconds here; use mode = \"job\" for long work)",
                    ));
                }
                Ok(VmState::Continue)
            },
        )
        .map_err(|error| error.to_string())?;
    }
    lua.set_app_data(Services {
        code: Arc::clone(code),
        permissions,
        cancel: Arc::clone(&cancel),
        budget: Rc::clone(&budget),
        job,
    });
    restrict_globals(&lua, code).map_err(|error| error.to_string())?;
    super::api::install(&lua).map_err(|error| error.to_string())?;
    let main = code
        .files
        .get(super::manifest::MAIN_FILE)
        .ok_or_else(|| format!("{} is missing", super::manifest::MAIN_FILE))?;
    budget.start(Some(QUICK_BUDGET));
    let module = lua
        .load(main.as_str())
        .set_name(format!("@{}", super::manifest::MAIN_FILE))
        .eval::<Value>()
        .map_err(|error| describe(&error));
    budget.start(None);
    let module = match module? {
        Value::Table(table) => table,
        Value::Nil => lua.create_table().map_err(|error| error.to_string())?,
        other => {
            return Err(format!(
                "{} must return a table (it returned a {})",
                super::manifest::MAIN_FILE,
                other.type_name()
            ));
        }
    };
    Ok(Vm {
        lua,
        module,
        budget,
        cancel,
    })
}

/// Removes everything that reaches outside the plugin and adds safe
/// replacements: `require` over the plugin's files, `print` to the log and
/// a small `os`.
fn restrict_globals(lua: &Lua, code: &Arc<PluginCode>) -> mlua::Result<()> {
    let globals = lua.globals();
    // File access and loading bytecode, which can crash the interpreter.
    for name in ["dofile", "loadfile", "load"] {
        globals.raw_set(name, Value::Nil)?;
    }
    if let Ok(string) = globals.get::<Table>("string") {
        string.raw_set("dump", Value::Nil)?;
    }
    // `load` for text only.
    globals.set(
        "load",
        lua.create_function(|lua, (chunk, name): (String, Option<String>)| {
            match lua
                .load(chunk)
                .set_name(name.unwrap_or_else(|| "=load".into()))
                .set_mode(mlua::ChunkMode::Text)
                .into_function()
            {
                Ok(function) => Ok((Value::Function(function), Value::Nil)),
                Err(error) => Ok((Value::Nil, Value::String(lua.create_string(error.to_string())?))),
            }
        })?,
    )?;
    let id = code.id().to_owned();
    globals.set(
        "print",
        lua.create_function(move |_, values: mlua::Variadic<Value>| {
            let text: Vec<String> = values.iter().map(display_value).collect();
            log::info!("[{id}] {}", text.join("\t"));
            Ok(())
        })?,
    )?;
    let loaded = lua.create_table()?;
    let files = code.files.clone();
    globals.set(
        "require",
        lua.create_function(move |lua, name: String| {
            if let Ok(value) = loaded.raw_get::<Value>(name.as_str())
                && !value.is_nil()
            {
                return Ok(value);
            }
            let file = module_file(&name)
                .ok_or_else(|| mlua::Error::runtime(format!("bad module name \"{name}\"")))?;
            let source = files.get(&file).ok_or_else(|| {
                mlua::Error::runtime(format!(
                    "module \"{name}\" not found (looked for {file} in the plugin folder)"
                ))
            })?;
            let value = lua
                .load(source.as_str())
                .set_name(format!("@{file}"))
                .set_mode(mlua::ChunkMode::Text)
                .call::<Value>(name.as_str())?;
            let value = if value.is_nil() { Value::Boolean(true) } else { value };
            loaded.raw_set(name.as_str(), value.clone())?;
            Ok(value)
        })?,
    )?;
    let os = lua.create_table()?;
    let started = Instant::now();
    os.set(
        "time",
        lua.create_function(|_, ()| {
            Ok(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs().cast_signed()))
        })?,
    )?;
    os.set(
        "clock",
        lua.create_function(move |_, ()| Ok(started.elapsed().as_secs_f64()))?,
    )?;
    os.set(
        "date",
        lua.create_function(|_, (format, time): (Option<String>, Option<i64>)| {
            let time = time.unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |duration| duration.as_secs().cast_signed())
            });
            let format = format.unwrap_or_else(|| "%Y-%m-%d %H:%M:%S".into());
            let format = format.strip_prefix('!').unwrap_or(&format).to_owned();
            let date = chrono::DateTime::from_timestamp(time, 0)
                .ok_or_else(|| mlua::Error::runtime("time out of range"))?;
            let items: Vec<chrono::format::Item<'_>> =
                chrono::format::StrftimeItems::new(&format).collect();
            if items.contains(&chrono::format::Item::Error) {
                return Err(mlua::Error::runtime(format!("bad date format \"{format}\"")));
            }
            Ok(date.format_with_items(items.into_iter()).to_string())
        })?,
    )?;
    globals.set("os", os)?;
    Ok(())
}

pub fn display_value(value: &Value) -> String {
    match value {
        Value::Nil => "nil".into(),
        Value::Boolean(value) => value.to_string(),
        Value::Integer(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.to_string_lossy(),
        other => format!("{}: {:p}", other.type_name(), other.to_pointer()),
    }
}

/// An error message without mlua's wrapping: the Lua message and where.
pub fn describe(error: &mlua::Error) -> String {
    match error {
        mlua::Error::CallbackError { cause, traceback } => {
            let cause = describe(cause);
            let place = traceback
                .lines()
                .map(str::trim)
                .find(|line| line.contains(".lua"))
                .map(|line| line.split(": in ").next().unwrap_or(line).to_owned());
            match place {
                Some(place) if !cause.contains(".lua:") => format!("{place}: {cause}"),
                _ => cause,
            }
        }
        mlua::Error::RuntimeError(message) | mlua::Error::SyntaxError { message, .. } => {
            message.lines().next().unwrap_or_default().to_owned()
        }
        mlua::Error::MemoryError(_) => "ran out of memory (the limit is 64 MB)".into(),
        mlua::Error::WithContext { context, cause } => format!("{context}: {}", describe(cause)),
        other => other.to_string().lines().next().unwrap_or_default().to_owned(),
    }
}

impl Vm {
    /// Builds the `ctx` table handlers receive.
    pub fn context(&self, context: &CallContext) -> mlua::Result<Table> {
        let lua = &self.lua;
        let table = lua.create_table()?;
        if let Some(dir) = &context.dir {
            table.set("dir", dir.to_string_lossy().as_ref())?;
            let root = self
                .services()
                .code
                .manifest
                .activation
                .root_for(dir);
            table.set("root", root.to_string_lossy().as_ref())?;
        }
        let targets = lua.create_table()?;
        for (index, target) in context.targets.iter().enumerate() {
            targets.raw_set(index + 1, target_table(lua, target)?)?;
        }
        table.set("targets", targets)?;
        if let Some(tab) = context.tab {
            table.set("tab_id", tab)?;
        }
        let values = context
            .values
            .as_ref()
            .map(|values| lua.to_value(values))
            .transpose()?
            .unwrap_or(Value::Table(lua.create_table()?));
        table.set("values", values)?;
        let plugin = lua.create_table()?;
        let manifest = &self.services().code.manifest;
        plugin.set("id", manifest.id.as_str())?;
        plugin.set("name", manifest.name.as_str())?;
        plugin.set("version", manifest.version.as_str())?;
        table.set("plugin", plugin)?;
        Ok(table)
    }

    fn services(&self) -> mlua::AppDataRef<'_, Services> {
        self.lua
            .app_data_ref::<Services>()
            .expect("plugin services are set when the state is created")
    }

    /// Looks up `M.<path...>`: `["actions", "convert", "form"]`. A function
    /// stored directly at `M.actions.convert` counts as its `run`.
    pub fn handler(&self, path: &[&str]) -> Option<mlua::Function> {
        let mut value = Value::Table(self.module.clone());
        for (index, key) in path.iter().enumerate() {
            let Value::Table(table) = value else {
                return None;
            };
            value = table.get::<Value>(*key).ok()?;
            if let Value::Function(function) = &value
                && index + 2 == path.len()
                && path[index + 1] == "run"
            {
                return Some(function.clone());
            }
        }
        match value {
            Value::Function(function) => Some(function),
            _ => None,
        }
    }

    /// Calls a handler within `budget`, turning errors into messages.
    pub fn call<A, R>(&self, function: &mlua::Function, args: A, budget: Option<Duration>) -> Result<R, String>
    where
        A: mlua::IntoLuaMulti,
        R: mlua::FromLuaMulti,
    {
        self.budget.start(budget);
        let result = function.call::<R>(args).map_err(|error| describe(&error));
        self.budget.start(None);
        // A cancelled call leaves the flag set for the next one otherwise.
        if result.is_err() && self.services().job.is_none() {
            self.cancel.store(false, Ordering::Release);
        }
        result
    }
}

fn target_table(lua: &Lua, target: &super::TargetInfo) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set("path", target.path.to_string_lossy().as_ref())?;
    table.set("name", target.name())?;
    let stem = target
        .path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    table.set("stem", if target.is_dir { target.name() } else { stem })?;
    table.set("ext", target.ext().to_ascii_lowercase())?;
    table.set("is_dir", target.is_dir)?;
    table.set("size", target.size)?;
    table.set("modified", target.modified)?;
    Ok(table)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::plugins::Source;

    pub fn code(main: &str, permissions: &[&str]) -> Arc<PluginCode> {
        code_with(main, permissions, &[])
    }

    pub fn code_with(main: &str, permissions: &[&str], files: &[(&str, &str)]) -> Arc<PluginCode> {
        let manifest = crate::plugins::manifest::parse(&format!(
            "api_version = 1\nid = \"io.example.test\"\nname = \"Test\"\nversion = \"1.0.0\"\nauthor = \"Me\"\npermissions = [{}]\n",
            permissions
                .iter()
                .map(|p| format!("\"{p}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .expect("manifest");
        let mut map = BTreeMap::new();
        map.insert("main.lua".to_owned(), main.to_owned());
        for (path, source) in files {
            map.insert((*path).to_owned(), (*source).to_owned());
        }
        Arc::new(PluginCode {
            manifest,
            files: map,
            source: Source::Bundled,
        })
    }

    use std::collections::BTreeMap;

    pub fn vm(main: &str, permissions: &[&str]) -> Vm {
        let code = code(main, permissions);
        let permissions = code.manifest.permissions.clone();
        create(
            &code,
            PermissionSet::from_manifest(&permissions),
            Arc::default(),
            None,
        )
        .expect("vm")
    }

    fn eval(vm: &Vm, expression: &str) -> Value {
        vm.lua
            .load(expression)
            .eval::<Value>()
            .expect("evaluates")
    }

    #[test]
    fn dangerous_globals_are_gone() {
        let vm = vm("return {}", &[]);
        for expression in [
            "io",
            "os.execute",
            "os.remove",
            "package",
            "debug",
            "dofile",
            "loadfile",
            "string.dump",
        ] {
            assert!(eval(&vm, expression).is_nil(), "{expression} is available");
        }
        assert!(eval(&vm, "os.time()").as_i64().is_some_and(|t| t > 0));
        assert!(!eval(&vm, "load('return 1')").is_nil());
    }

    #[test]
    fn require_sees_only_plugin_files() {
        let code = code_with(
            "local args = require('lib.args'); return { value = args.answer }",
            &[],
            &[("lib/args.lua", "return { answer = 42 }")],
        );
        let vm = create(&code, PermissionSet::default(), Arc::default(), None).expect("vm");
        assert_eq!(vm.module.get::<i64>("value").expect("value"), 42);
        assert!(vm.lua.load("require('../secret')").exec().is_err());
        assert!(vm.lua.load("require('missing')").exec().is_err());
    }

    #[test]
    fn runaway_code_is_stopped_by_the_budget() {
        let vm = vm("return { spin = function() while true do end end }", &[]);
        let spin = vm.handler(&["spin"]).expect("handler");
        let started = Instant::now();
        let error = vm
            .call::<_, ()>(&spin, (), Some(Duration::from_millis(200)))
            .expect_err("stopped");
        assert!(error.contains("took too long"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn memory_is_capped() {
        let vm = vm(
            "return { grow = function() local t = {} for i = 1, 1e9 do t[i] = string.rep('x', 1024) .. i end end }",
            &[],
        );
        let grow = vm.handler(&["grow"]).expect("handler");
        let error = vm.call::<_, ()>(&grow, (), None).expect_err("out of memory");
        assert!(error.contains("memory"), "{error}");
    }

    #[test]
    fn main_must_return_a_table_and_errors_are_described() {
        let plugin = code("return 5", &[]);
        let error = create(&plugin, PermissionSet::default(), Arc::default(), None)
            .err()
            .expect("not a table");
        assert!(error.contains("must return a table"), "{error}");
        let plugin = code("local x = = 1", &[]);
        let error = create(&plugin, PermissionSet::default(), Arc::default(), None)
            .err()
            .expect("syntax");
        assert!(error.contains("main.lua:1"), "{error}");
    }

    #[test]
    fn handlers_resolve_functions_and_run_shorthand() {
        let vm = vm(
            "return { actions = { a = function() end, b = { run = function() end, form = function() end } } }",
            &[],
        );
        assert!(vm.handler(&["actions", "a", "run"]).is_some());
        assert!(vm.handler(&["actions", "a", "form"]).is_none());
        assert!(vm.handler(&["actions", "b", "form"]).is_some());
        assert!(vm.handler(&["actions", "c", "run"]).is_none());
    }
}
