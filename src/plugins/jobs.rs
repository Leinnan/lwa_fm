//! Long-running actions (`mode = "job"`). Each job gets a thread and a fresh
//! Lua state, so the plugin's worker keeps answering the bar meanwhile.
//! Jobs have no time limit; Cancel stops the Lua code and kills any process
//! it started.

use super::permissions::PermissionSet;
use super::sandbox::{self, JobLink};
use super::{CallContext, JobEvent, PluginCode, PluginMsg, send};
use mlua::{LuaSerdeExt as _, Table, Value};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct JobSpec {
    pub id: u64,
    pub code: Arc<PluginCode>,
    pub permissions: PermissionSet,
    pub action: String,
    pub context: CallContext,
    pub per_item: bool,
    pub cancel: Arc<AtomicBool>,
}

/// How one call went.
enum ItemResult {
    Done,
    Skipped,
    Failed(String),
}

/// `nil`/`true` is done; `false`, `"skipped"` or `{ skipped = true }` is
/// skipped (a string or `message` says why, and is logged).
fn item_result(value: &Value) -> ItemResult {
    match value {
        Value::Boolean(false) => ItemResult::Skipped,
        Value::String(text) if text.to_string_lossy().starts_with("skip") => ItemResult::Skipped,
        Value::Table(table) if table.get::<bool>("skipped").unwrap_or(false) => {
            ItemResult::Skipped
        }
        Value::Table(table) => match table.get::<Option<String>>("error") {
            Ok(Some(error)) => ItemResult::Failed(error),
            _ => ItemResult::Done,
        },
        _ => ItemResult::Done,
    }
}

pub fn spawn(spec: JobSpec) {
    let name = format!("plugin job {}", spec.code.id());
    let id = spec.id;
    let spawned = std::thread::Builder::new()
        .name(name)
        .spawn(move || run(spec));
    if let Err(error) = spawned {
        send(PluginMsg::Job {
            id,
            event: JobEvent::Finished {
                done: 0,
                skipped: 0,
                failures: vec![format!("could not start: {error}")],
                cancelled: false,
            },
        });
    }
}

#[allow(clippy::too_many_lines)]
fn run(spec: JobSpec) {
    let JobSpec {
        id,
        code,
        permissions,
        action,
        context,
        per_item,
        cancel,
    } = spec;
    let finish = |done, skipped, failures: Vec<String>| {
        send(PluginMsg::Job {
            id,
            event: JobEvent::Finished {
                done,
                skipped,
                failures,
                cancelled: cancel.load(Ordering::Acquire),
            },
        });
    };
    let vm = match sandbox::create(&code, permissions, Arc::clone(&cancel), Some(JobLink::new(id))) {
        Ok(vm) => vm,
        Err(error) => {
            finish(0, 0, vec![error]);
            return;
        }
    };
    let link = || {
        vm.lua
            .app_data_ref::<sandbox::Services>()
            .expect("plugin services")
    };
    let prepared = (|| -> Result<(Table, Value), String> {
        let ctx = vm.context(&context).map_err(|e| e.to_string())?;
        let values = match &context.values {
            Some(values) => vm.lua.to_value(values).map_err(|e| e.to_string())?,
            None => Value::Table(vm.lua.create_table().map_err(|e| e.to_string())?),
        };
        Ok((ctx, values))
    })();
    let (ctx, values) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            finish(0, 0, vec![error]);
            return;
        }
    };
    if !per_item {
        let Some(function) = vm.handler(&["actions", &action, "run"]) else {
            finish(0, 0, vec![format!("no function M.actions.{action} (or .run)")]);
            return;
        };
        if let Some(job) = &link().job {
            job.report(0.0, Some(String::new()), true);
        }
        match vm.call::<_, Value>(&function, (ctx, values), None) {
            Ok(value) => match item_result(&value) {
                ItemResult::Done => finish(1, 0, Vec::new()),
                ItemResult::Skipped => finish(0, 1, Vec::new()),
                ItemResult::Failed(error) => finish(0, 0, vec![error]),
            },
            Err(error) => finish(0, 0, vec![error]),
        }
        return;
    }
    let Some(function) = vm.handler(&["actions", &action, "run_item"]) else {
        finish(
            0,
            0,
            vec![format!("per_item needs a function M.actions.{action}.run_item")],
        );
        return;
    };
    let Ok(targets) = ctx.get::<Table>("targets") else {
        finish(0, 0, vec!["no targets".into()]);
        return;
    };
    let count = context.targets.len().max(1);
    let (mut done, mut skipped, mut failures) = (0, 0, Vec::new());
    for (index, target) in context.targets.iter().enumerate() {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        if let Some(job) = &link().job {
            job.base.set(index as f32 / count as f32);
            job.scale.set(1.0 / count as f32);
            let label = if count > 1 {
                format!("{} ({}/{count})", target.name(), index + 1)
            } else {
                target.name()
            };
            job.report(0.0, Some(label), true);
        }
        let item: Value = targets.get(index + 1).unwrap_or(Value::Nil);
        match vm.call::<_, Value>(&function, (ctx.clone(), item, values.clone()), None) {
            Ok(value) => match item_result(&value) {
                ItemResult::Done => done += 1,
                ItemResult::Skipped => skipped += 1,
                ItemResult::Failed(error) => failures.push(format!("{}: {error}", target.name())),
            },
            Err(_) if cancel.load(Ordering::Acquire) => break,
            Err(error) => failures.push(format!("{}: {error}", target.name())),
        }
    }
    if let Some(job) = &link().job {
        job.base.set(0.0);
        job.scale.set(1.0);
        job.report(1.0, None, true);
    }
    finish(done, skipped, failures);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::sandbox::tests::vm;

    #[test]
    fn item_results() {
        let vm = vm("return {}", &[]);
        let eval = |code: &str| vm.lua.load(code).eval::<Value>().expect("value");
        assert!(matches!(item_result(&eval("return nil")), ItemResult::Done));
        assert!(matches!(item_result(&eval("return false")), ItemResult::Skipped));
        assert!(matches!(item_result(&eval("return 'skipped: exists'")), ItemResult::Skipped));
        assert!(matches!(item_result(&eval("return { skipped = true }")), ItemResult::Skipped));
        assert!(matches!(item_result(&eval("return { error = 'bad' }")), ItemResult::Failed(e) if e == "bad"));
    }
}
