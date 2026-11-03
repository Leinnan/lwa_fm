//! The `dirfleet` table: everything a plugin can do. Functions that touch
//! the system check the plugin's approved permissions first; a missing
//! permission raises an error naming it. Failures of the operation itself
//! return `nil, message`, the Lua convention.

use super::permissions::Permission;
use super::sandbox::Services;
use super::{AppRequest, NotifyLevel, PluginMsg, send};
use crate::child_process::{self, RunError};
use crate::fileops::{ConflictPolicy, FileOp};
use mlua::{Function, Lua, LuaSerdeExt as _, MultiValue, Table, Value, Variadic};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Largest file `fs.read_text` reads by default.
const READ_LIMIT: usize = 1024 * 1024;
/// Most entries `fs.read_dir` and `fs.glob` return.
const LIST_LIMIT: usize = 10_000;
/// Bytes of a process's stdout and stderr kept.
const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;

pub fn install(lua: &Lua) -> mlua::Result<()> {
    let dirfleet = lua.create_table()?;
    dirfleet.set("version", env!("CARGO_PKG_VERSION"))?;
    dirfleet.set("api_version", 1)?;
    dirfleet.set("os", std::env::consts::OS)?;
    dirfleet.set("path", path_table(lua)?)?;
    dirfleet.set("fmt", fmt_table(lua)?)?;
    dirfleet.set("fs", fs_table(lua)?)?;
    dirfleet.set("ops", ops_table(lua)?)?;
    dirfleet.set("process", process_table(lua)?)?;
    dirfleet.set("ffmpeg", ffmpeg_table(lua)?)?;
    dirfleet.set("media", media_table(lua)?)?;
    dirfleet.set("app", app_table(lua)?)?;
    dirfleet.set("clipboard", clipboard_table(lua)?)?;
    dirfleet.set("log", log_table(lua)?)?;
    dirfleet.set("job", job_table(lua)?)?;
    dirfleet.set("storage", storage_table(lua)?)?;
    dirfleet.set("bar", invalidate_table(lua, true)?)?;
    dirfleet.set("inspector", invalidate_table(lua, false)?)?;
    dirfleet.set(
        "notify",
        lua.create_function(|lua, (text, level): (String, Option<String>)| {
            let level = match level.as_deref() {
                Some("success") => NotifyLevel::Success,
                Some("warning" | "warn") => NotifyLevel::Warning,
                Some("error") => NotifyLevel::Error,
                _ => NotifyLevel::Info,
            };
            let name = services(lua)?.code.manifest.name.clone();
            send(PluginMsg::App(AppRequest::Notify {
                level,
                text: format!("{name}: {text}"),
            }));
            Ok(())
        })?,
    )?;
    lua.globals().set("dirfleet", dirfleet)
}

fn services(lua: &Lua) -> mlua::Result<mlua::AppDataRef<'_, Services>> {
    lua.app_data_ref::<Services>()
        .ok_or_else(|| mlua::Error::runtime("plugin services are missing"))
}

fn require(lua: &Lua, permission: &Permission) -> mlua::Result<()> {
    services(lua)?
        .permissions
        .require(permission)
        .map_err(mlua::Error::runtime)
}

fn plugin_id(lua: &Lua) -> mlua::Result<String> {
    Ok(services(lua)?.code.id().to_owned())
}

/// Runs `f`, which waits outside Lua, without spending the time budget.
fn paused<R>(lua: &Lua, f: impl FnOnce() -> R) -> mlua::Result<R> {
    let budget = std::rc::Rc::clone(&services(lua)?.budget);
    Ok(budget.paused(f))
}

/// `nil, message`.
fn failure(lua: &Lua, message: impl std::fmt::Display) -> mlua::Result<MultiValue> {
    Ok(MultiValue::from_vec(vec![
        Value::Nil,
        Value::String(lua.create_string(message.to_string())?),
    ]))
}

fn absolute(path: &str) -> mlua::Result<PathBuf> {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(mlua::Error::runtime(format!(
            "\"{}\" is not an absolute path",
            path.display()
        )))
    }
}

fn to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// A string or a list of strings.
fn paths_arg(value: Value) -> mlua::Result<Vec<PathBuf>> {
    match value {
        Value::String(text) => Ok(vec![absolute(&text.to_str()?)?]),
        Value::Table(table) => table
            .sequence_values::<String>()
            .map(|path| absolute(&path?))
            .collect(),
        other => Err(mlua::Error::runtime(format!(
            "expected a path or a list of paths, got {}",
            other.type_name()
        ))),
    }
}

fn unix_seconds(time: std::io::Result<std::time::SystemTime>) -> i64 {
    time.ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs().cast_signed())
}

fn path_table(lua: &Lua) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set(
        "join",
        lua.create_function(|_, parts: Variadic<String>| {
            let mut path = PathBuf::new();
            for part in parts.iter() {
                path.push(part);
            }
            Ok(to_string(&path))
        })?,
    )?;
    table.set(
        "parent",
        lua.create_function(|_, path: String| Ok(Path::new(&path).parent().map(to_string)))?,
    )?;
    table.set(
        "name",
        lua.create_function(|_, path: String| {
            Ok(Path::new(&path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default())
        })?,
    )?;
    table.set(
        "stem",
        lua.create_function(|_, path: String| {
            Ok(Path::new(&path)
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default())
        })?,
    )?;
    table.set(
        "ext",
        lua.create_function(|_, path: String| {
            Ok(Path::new(&path)
                .extension()
                .map(|ext| ext.to_string_lossy().into_owned())
                .unwrap_or_default())
        })?,
    )?;
    table.set(
        "with_ext",
        lua.create_function(|_, (path, ext): (String, String)| {
            Ok(to_string(
                &Path::new(&path).with_extension(ext.trim_start_matches('.')),
            ))
        })?,
    )?;
    table.set(
        "is_absolute",
        lua.create_function(|_, path: String| Ok(Path::new(&path).is_absolute()))?,
    )?;
    table.set(
        "unique",
        lua.create_function(|_, path: String| Ok(to_string(&unique_path(Path::new(&path)))))?,
    )?;
    Ok(table)
}

/// `path` when free, otherwise `stem (2).ext`, `stem (3).ext`, …
pub fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy()))
        .unwrap_or_default();
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    (2..10_000)
        .map(|n| parent.join(format!("{stem} ({n}){ext}")))
        .find(|candidate| !candidate.exists())
        .unwrap_or_else(|| path.to_path_buf())
}

pub fn format_duration(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    let (hours, minutes, secs) = (total / 3600, total / 60 % 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes}:{secs:02}")
    }
}

fn fmt_table(lua: &Lua) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set(
        "duration",
        lua.create_function(|_, seconds: Option<f64>| Ok(seconds.map(format_duration)))?,
    )?;
    table.set(
        "size",
        lua.create_function(|_, bytes: u64| Ok(crate::helper::format_bytes_simple(bytes)))?,
    )?;
    Ok(table)
}

fn stat_table(lua: &Lua, path: &Path, metadata: &std::fs::Metadata) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set("path", to_string(path))?;
    table.set(
        "name",
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
    )?;
    table.set("is_dir", metadata.is_dir())?;
    table.set("is_file", metadata.is_file())?;
    table.set("size", metadata.len())?;
    table.set("modified", unix_seconds(metadata.modified()))?;
    table.set("readonly", metadata.permissions().readonly())?;
    Ok(table)
}

#[allow(clippy::too_many_lines)]
fn fs_table(lua: &Lua) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    let read = |lua: &Lua| require(lua, &Permission::FsRead);
    table.set(
        "exists",
        lua.create_function(move |lua, path: String| {
            read(lua)?;
            Ok(absolute(&path)?.exists())
        })?,
    )?;
    table.set(
        "is_dir",
        lua.create_function(move |lua, path: String| {
            read(lua)?;
            Ok(absolute(&path)?.is_dir())
        })?,
    )?;
    table.set(
        "is_file",
        lua.create_function(move |lua, path: String| {
            read(lua)?;
            Ok(absolute(&path)?.is_file())
        })?,
    )?;
    table.set(
        "stat",
        lua.create_function(move |lua, path: String| {
            read(lua)?;
            let path = absolute(&path)?;
            match paused(lua, || std::fs::metadata(&path))? {
                Ok(metadata) => Ok(MultiValue::from_vec(vec![Value::Table(stat_table(
                    lua, &path, &metadata,
                )?)])),
                Err(error) => failure(lua, error),
            }
        })?,
    )?;
    table.set(
        "read_dir",
        lua.create_function(move |lua, path: String| {
            read(lua)?;
            let path = absolute(&path)?;
            let entries = paused(lua, || {
                std::fs::read_dir(&path).map(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .take(LIST_LIMIT)
                        .filter_map(|entry| Some((entry.path(), entry.metadata().ok()?)))
                        .collect::<Vec<_>>()
                })
            })?;
            match entries {
                Ok(entries) => {
                    let list = lua.create_table()?;
                    for (index, (path, metadata)) in entries.iter().enumerate() {
                        list.raw_set(index + 1, stat_table(lua, path, metadata)?)?;
                    }
                    Ok(MultiValue::from_vec(vec![Value::Table(list)]))
                }
                Err(error) => failure(lua, error),
            }
        })?,
    )?;
    table.set(
        "read_text",
        lua.create_function(move |lua, (path, max): (String, Option<usize>)| {
            read(lua)?;
            let path = absolute(&path)?;
            let max = max.unwrap_or(READ_LIMIT).min(64 * READ_LIMIT);
            let bytes = paused(lua, || {
                use std::io::Read as _;
                let mut bytes = Vec::new();
                std::fs::File::open(&path)
                    .and_then(|file| file.take(max as u64 + 1).read_to_end(&mut bytes))
                    .map(|_| bytes)
            })?;
            match bytes {
                Ok(mut bytes) => {
                    let truncated = bytes.len() > max;
                    bytes.truncate(max);
                    Ok(MultiValue::from_vec(vec![
                        Value::String(lua.create_string(String::from_utf8_lossy(&bytes).as_ref())?),
                        Value::Boolean(truncated),
                    ]))
                }
                Err(error) => failure(lua, error),
            }
        })?,
    )?;
    table.set(
        "glob",
        lua.create_function(move |lua, pattern: String| {
            read(lua)?;
            absolute(&pattern)?;
            let paths = paused(lua, || {
                glob::glob(&pattern).map(|paths| {
                    paths
                        .filter_map(Result::ok)
                        .take(LIST_LIMIT)
                        .map(|path| to_string(&path))
                        .collect::<Vec<_>>()
                })
            })?;
            match paths {
                Ok(paths) => Ok(MultiValue::from_vec(vec![Value::Table(
                    lua.create_sequence_from(paths)?,
                )])),
                Err(error) => failure(lua, error),
            }
        })?,
    )?;
    let write = |lua: &Lua| require(lua, &Permission::FsWrite);
    table.set(
        "write_text",
        lua.create_function(move |lua, (path, text): (String, mlua::String)| {
            write(lua)?;
            let path = absolute(&path)?;
            let bytes = text.as_bytes().to_vec();
            match paused(lua, || std::fs::write(&path, bytes))? {
                Ok(()) => Ok(MultiValue::from_vec(vec![Value::Boolean(true)])),
                Err(error) => failure(lua, error),
            }
        })?,
    )?;
    table.set(
        "mkdir",
        lua.create_function(move |lua, path: String| {
            write(lua)?;
            let path = absolute(&path)?;
            match paused(lua, || std::fs::create_dir_all(&path))? {
                Ok(()) => Ok(MultiValue::from_vec(vec![Value::Boolean(true)])),
                Err(error) => failure(lua, error),
            }
        })?,
    )?;
    Ok(table)
}

fn policy(text: Option<&str>) -> mlua::Result<ConflictPolicy> {
    match text {
        None | Some("keep_both") => Ok(ConflictPolicy::KeepBoth),
        Some("replace") => Ok(ConflictPolicy::Replace),
        Some("skip") => Ok(ConflictPolicy::Skip),
        Some(other) => Err(mlua::Error::runtime(format!(
            "unknown conflict policy \"{other}\"; use keep_both, replace or skip"
        ))),
    }
}

/// File operations go through the app's worker, so they show progress,
/// handle conflicts and can be undone.
fn ops_table(lua: &Lua) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    let write = |lua: &Lua| require(lua, &Permission::FsWrite);
    table.set(
        "copy",
        lua.create_function(
            move |lua, (srcs, dst, conflict): (Value, String, Option<String>)| {
                write(lua)?;
                send(PluginMsg::App(AppRequest::FileOp(FileOp::Copy {
                    srcs: paths_arg(srcs)?,
                    dst: absolute(&dst)?,
                    policy: policy(conflict.as_deref())?,
                })));
                Ok(())
            },
        )?,
    )?;
    table.set(
        "move",
        lua.create_function(
            move |lua, (srcs, dst, conflict): (Value, String, Option<String>)| {
                write(lua)?;
                send(PluginMsg::App(AppRequest::FileOp(FileOp::Move {
                    srcs: paths_arg(srcs)?,
                    dst: absolute(&dst)?,
                    policy: policy(conflict.as_deref())?,
                })));
                Ok(())
            },
        )?,
    )?;
    table.set(
        "trash",
        lua.create_function(move |lua, paths: Value| {
            write(lua)?;
            send(PluginMsg::App(AppRequest::FileOp(FileOp::Trash(
                paths_arg(paths)?,
            ))));
            Ok(())
        })?,
    )?;
    table.set(
        "rename",
        lua.create_function(move |lua, (from, to): (String, String)| {
            write(lua)?;
            let from = absolute(&from)?;
            // A bare name renames in place.
            let to = if Path::new(&to).is_absolute() {
                PathBuf::from(to)
            } else if to.contains(['/', '\\']) {
                return Err(mlua::Error::runtime(
                    "rename takes a new name or an absolute path",
                ));
            } else {
                from.with_file_name(to)
            };
            send(PluginMsg::App(AppRequest::FileOp(FileOp::Rename {
                from,
                to,
            })));
            Ok(())
        })?,
    )?;
    Ok(table)
}

struct ProcessOptions {
    args: Vec<String>,
    cwd: Option<PathBuf>,
    env: Vec<(String, String)>,
    timeout: Option<Duration>,
    stdin: Option<Vec<u8>>,
    on_line: Option<Function>,
}

fn process_options(table: &Table) -> mlua::Result<ProcessOptions> {
    let args = match table.get::<Value>("args")? {
        Value::Nil => Vec::new(),
        Value::Table(args) => args
            .sequence_values::<Value>()
            .map(|value| match value? {
                Value::String(text) => Ok(text.to_str()?.to_owned()),
                Value::Integer(number) => Ok(number.to_string()),
                Value::Number(number) => Ok(number.to_string()),
                other => Err(mlua::Error::runtime(format!(
                    "process arguments must be strings, got {}",
                    other.type_name()
                ))),
            })
            .collect::<mlua::Result<_>>()?,
        other => {
            return Err(mlua::Error::runtime(format!(
                "args must be a list, got {}",
                other.type_name()
            )));
        }
    };
    let cwd = table
        .get::<Option<String>>("cwd")?
        .map(|cwd| absolute(&cwd))
        .transpose()?;
    let env = table
        .get::<Option<Table>>("env")?
        .map(|env| {
            env.pairs::<String, String>()
                .collect::<mlua::Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    let timeout = table
        .get::<Option<f64>>("timeout")?
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .map(Duration::from_secs_f64);
    let stdin = table
        .get::<Option<mlua::String>>("stdin")?
        .map(|text| text.as_bytes().to_vec());
    Ok(ProcessOptions {
        args,
        cwd,
        env,
        timeout,
        stdin,
        on_line: table.get("on_line")?,
    })
}

/// Runs a command for a plugin: streaming lines to `on_line`, cancelled
/// with the plugin's call, and not counted against its time budget.
fn run_process(
    lua: &Lua,
    command: &mut Command,
    options: &ProcessOptions,
    mut extra_line: impl FnMut(&str),
) -> mlua::Result<Result<child_process::Output, RunError>> {
    let (cancel, default_timeout) = {
        let services = services(lua)?;
        (
            std::sync::Arc::clone(&services.cancel),
            services.budget.default_timeout(),
        )
    };
    command.args(&options.args);
    if let Some(cwd) = &options.cwd {
        command.current_dir(cwd);
    }
    for (key, value) in &options.env {
        command.env(key, value);
    }
    let deadline = options
        .timeout
        .or(default_timeout)
        .map(|timeout| Instant::now() + timeout);
    let mut callback_error = None;
    let mut on_line = |line: &str| {
        extra_line(line);
        if let Some(function) = &options.on_line
            && callback_error.is_none()
            && let Err(error) = function.call::<()>(line)
        {
            callback_error = Some(error);
        }
    };
    let result = paused(lua, || {
        child_process::run(
            command,
            child_process::Run {
                cancel: &cancel,
                deadline,
                stdin: options.stdin.clone(),
                output_limit: OUTPUT_LIMIT,
                on_line: Some(&mut on_line),
            },
        )
    })?;
    if let Some(error) = callback_error {
        return Err(error);
    }
    if matches!(result, Err(RunError::Cancelled)) || cancel.load(Ordering::Acquire) {
        return Err(mlua::Error::runtime("cancelled"));
    }
    Ok(result)
}

fn output_table(lua: &Lua, output: &child_process::Output) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set("code", output.code)?;
    table.set("ok", output.code == Some(0))?;
    table.set("stdout", output.stdout.as_str())?;
    table.set("stderr", output.stderr.as_str())?;
    Ok(table)
}

fn process_table(lua: &Lua) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set(
        "run",
        lua.create_function(|lua, options: Table| {
            let program: String = options
                .get::<Option<String>>("cmd")?
                .ok_or_else(|| mlua::Error::runtime("process.run needs cmd"))?;
            if !services(lua)?.permissions.allows_program(&program) {
                let name = Path::new(&program)
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                return Err(mlua::Error::runtime(format!(
                    "running \"{program}\" needs the \"process:{name}\" permission; add it to plugin.toml"
                )));
            }
            let parsed = process_options(&options)?;
            let mut command = Command::new(&program);
            match run_process(lua, &mut command, &parsed, |_| {})? {
                Ok(output) => Ok(MultiValue::from_vec(vec![Value::Table(output_table(
                    lua, &output,
                )?)])),
                Err(error) => failure(lua, error),
            }
        })?,
    )?;
    Ok(table)
}

/// `FFmpeg` and `FFprobe`, waiting (in jobs) for the download when they
/// are missing.
fn ffmpeg_tools(lua: &Lua) -> mlua::Result<(PathBuf, PathBuf)> {
    require(lua, &Permission::Ffmpeg)?;
    let (ffmpeg, ffprobe) = crate::app::assets::ffmpeg_tool_paths();
    if ffmpeg.is_file() {
        return Ok((ffmpeg, ffprobe));
    }
    let (in_job, cancel) = {
        let services = services(lua)?;
        (
            services.job.is_some(),
            std::sync::Arc::clone(&services.cancel),
        )
    };
    if !in_job {
        return Ok((ffmpeg, ffprobe));
    }
    if let Some(job) = &services(lua)?.job {
        job.report(0.0, Some("Waiting for FFmpeg to download…".into()), true);
    }
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(180) && !cancel.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(500));
        let tools = crate::app::assets::ffmpeg_tool_paths();
        if tools.0.is_file() {
            return Ok(tools);
        }
    }
    Ok(crate::app::assets::ffmpeg_tool_paths())
}

/// The `FFmpeg` progress value in seconds, from a `-progress` line.
fn progress_seconds(line: &str) -> Option<f64> {
    let (key, value) = line.split_once('=')?;
    match key {
        // Both are in microseconds despite the name.
        "out_time_us" | "out_time_ms" => value.trim().parse::<f64>().ok().map(|us| us / 1e6),
        _ => None,
    }
}

/// Encoder names, by the `FFmpeg` they came from.
type EncoderCache = Option<(PathBuf, Vec<String>)>;
static ENCODERS: LazyLock<Mutex<EncoderCache>> = LazyLock::new(Mutex::default);

#[allow(clippy::too_many_lines)]
fn ffmpeg_table(lua: &Lua) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set(
        "available",
        lua.create_function(|lua, ()| {
            require(lua, &Permission::Ffmpeg)?;
            Ok(crate::app::assets::ffmpeg_tool_paths().0.is_file())
        })?,
    )?;
    table.set(
        "run",
        lua.create_function(|lua, options: Table| {
            let (ffmpeg, _) = ffmpeg_tools(lua)?;
            let duration = options
                .get::<Option<f64>>("duration")?
                .filter(|duration| *duration > 0.0);
            let on_progress: Option<Function> = options.get("on_progress")?;
            let parsed = process_options(&options)?;
            let mut command = Command::new(&ffmpeg);
            command.args([
                "-hide_banner",
                "-nostdin",
                "-nostats",
                "-loglevel",
                "error",
                "-progress",
                "pipe:1",
            ]);
            let job = {
                let services = services(lua)?;
                services.job.as_ref().map(|job| job.id)
            };
            let mut progress_error = None;
            let lua_ref = lua.clone();
            let report = |line: &str| {
                let fraction = if line == "progress=end" {
                    Some(1.0)
                } else {
                    progress_seconds(line)
                        .zip(duration)
                        .map(|(seconds, duration)| (seconds / duration).clamp(0.0, 1.0))
                };
                let Some(fraction) = fraction else {
                    return;
                };
                if job.is_some()
                    && let Ok(services) = services(&lua_ref)
                    && let Some(link) = &services.job
                {
                    link.report(fraction as f32, None, false);
                }
                if let Some(function) = &on_progress
                    && progress_error.is_none()
                    && let Err(error) = function.call::<()>(fraction)
                {
                    progress_error = Some(error);
                }
            };
            let result = run_process(lua, &mut command, &parsed, report)?;
            match result {
                Ok(output) => {
                    let table = lua.create_table()?;
                    table.set("code", output.code)?;
                    table.set("ok", output.code == Some(0))?;
                    table.set("stderr", output.stderr.trim())?;
                    Ok(MultiValue::from_vec(vec![Value::Table(table)]))
                }
                Err(RunError::NotFound(_)) => failure(
                    lua,
                    "FFmpeg isn't installed yet; it is downloaded when first needed",
                ),
                Err(error) => failure(lua, error),
            }
        })?,
    )?;
    table.set(
        "version",
        lua.create_function(|lua, ()| {
            let (ffmpeg, _) = ffmpeg_tools(lua)?;
            let output = paused(lua, || {
                child_process::run(
                    Command::new(&ffmpeg).arg("-version"),
                    child_process::Run {
                        cancel: &std::sync::atomic::AtomicBool::new(false),
                        deadline: Some(Instant::now() + Duration::from_secs(10)),
                        stdin: None,
                        output_limit: 64 * 1024,
                        on_line: None,
                    },
                )
            })?;
            Ok(output
                .ok()
                .and_then(|output| output.stdout.lines().next().map(str::to_owned)))
        })?,
    )?;
    table.set(
        "encoders",
        lua.create_function(|lua, ()| {
            let (ffmpeg, _) = ffmpeg_tools(lua)?;
            let cached = ENCODERS
                .lock()
                .ok()
                .and_then(|cache| cache.clone())
                .filter(|(path, _)| *path == ffmpeg)
                .map(|(_, names)| names);
            let names = if let Some(names) = cached {
                names
            } else {
                let output = paused(lua, || {
                    child_process::run(
                        Command::new(&ffmpeg).args(["-hide_banner", "-encoders"]),
                        child_process::Run {
                            cancel: &std::sync::atomic::AtomicBool::new(false),
                            deadline: Some(Instant::now() + Duration::from_secs(10)),
                            stdin: None,
                            output_limit: 1024 * 1024,
                            on_line: None,
                        },
                    )
                })?;
                let names = output
                    .map(|output| parse_encoders(&output.stdout))
                    .unwrap_or_default();
                if let Ok(mut cache) = ENCODERS.lock()
                    && !names.is_empty()
                {
                    *cache = Some((ffmpeg, names.clone()));
                }
                names
            };
            let table = lua.create_table()?;
            for name in names {
                table.set(name, true)?;
            }
            Ok(table)
        })?,
    )?;
    Ok(table)
}

/// Encoder names from `ffmpeg -encoders`: lines like ` V....D libx264  …`.
fn parse_encoders(text: &str) -> Vec<String> {
    text.lines()
        .skip_while(|line| !line.trim_start().starts_with("------"))
        .skip(1)
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let flags = parts.next()?;
            (flags.len() == 6).then(|| parts.next().map(str::to_owned))?
        })
        .collect()
}

/// What `media.probe` learns about a media file.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct MediaInfo {
    pub duration: Option<f64>,
    pub width: Option<u64>,
    pub height: Option<u64>,
    pub fps: Option<f64>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub has_video: bool,
    pub has_audio: bool,
    pub rotation: Option<i64>,
    pub format: Option<String>,
    pub bit_rate: Option<u64>,
}

pub fn parse_probe(json: &str) -> Option<MediaInfo> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let streams = value["streams"].as_array().cloned().unwrap_or_default();
    let video = streams.iter().find(|stream| {
        stream["codec_type"] == "video"
            && stream["disposition"]["attached_pic"].as_u64().unwrap_or(0) == 0
    });
    let audio = streams
        .iter()
        .find(|stream| stream["codec_type"] == "audio");
    let number = |value: &serde_json::Value| -> Option<f64> {
        value
            .as_f64()
            .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
            .filter(|n: &f64| n.is_finite() && *n > 0.0)
    };
    let fps = video.and_then(|stream| {
        let text = stream["avg_frame_rate"].as_str()?;
        let (n, d) = text.split_once('/')?;
        let fps = n.parse::<f64>().ok()? / d.parse::<f64>().ok()?;
        (fps.is_finite() && fps > 0.0).then_some(fps)
    });
    let rotation = video.and_then(|stream| {
        stream["side_data_list"]
            .as_array()?
            .iter()
            .find_map(|data| data["rotation"].as_i64())
            .or_else(|| stream["tags"]["rotate"].as_str()?.parse().ok())
    });
    Some(MediaInfo {
        duration: number(&value["format"]["duration"])
            .or_else(|| video.and_then(|stream| number(&stream["duration"]))),
        width: video.and_then(|stream| stream["width"].as_u64()),
        height: video.and_then(|stream| stream["height"].as_u64()),
        fps,
        video_codec: video.and_then(|stream| stream["codec_name"].as_str().map(str::to_owned)),
        audio_codec: audio.and_then(|stream| stream["codec_name"].as_str().map(str::to_owned)),
        has_video: video.is_some(),
        has_audio: audio.is_some(),
        rotation,
        format: value["format"]["format_name"].as_str().map(str::to_owned),
        bit_rate: number(&value["format"]["bit_rate"]).map(|rate| rate as u64),
    })
}

type ProbeKey = (PathBuf, u64, i64);
static PROBES: LazyLock<Mutex<lru::LruCache<ProbeKey, MediaInfo>>> = LazyLock::new(|| {
    Mutex::new(lru::LruCache::new(
        std::num::NonZeroUsize::new(256).expect("probe cache capacity"),
    ))
});

/// `MediaInfo` for Lua, with unknown fields nil: the default `lua.null()` is
/// truthy, so `info.audio_codec or "None"` would keep it.
fn media_info_value(lua: &Lua, info: &MediaInfo) -> mlua::Result<Value> {
    lua.to_value_with(
        info,
        mlua::SerializeOptions::new().serialize_none_to_null(false),
    )
}

fn media_table(lua: &Lua) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set(
        "probe",
        lua.create_function(|lua, path: String| {
            let path = absolute(&path)?;
            let (_, ffprobe) = ffmpeg_tools(lua)?;
            let Ok(metadata) = std::fs::metadata(&path) else {
                return failure(lua, format!("{} doesn't exist", path.display()));
            };
            let key = (path.clone(), metadata.len(), unix_seconds(metadata.modified()));
            if let Some(info) = PROBES.lock().ok().and_then(|mut cache| cache.get(&key).cloned()) {
                return Ok(MultiValue::from_vec(vec![media_info_value(lua, &info)?]));
            }
            let cancel = std::sync::Arc::clone(&services(lua)?.cancel);
            let output = paused(lua, || {
                child_process::run(
                    Command::new(&ffprobe)
                        .args([
                            "-v",
                            "error",
                            "-show_entries",
                            "format=duration,format_name,bit_rate:stream=codec_type,codec_name,width,height,avg_frame_rate,duration:stream_disposition=attached_pic:stream_tags=rotate:stream_side_data=rotation",
                            "-of",
                            "json",
                        ])
                        .arg(&path),
                    child_process::Run {
                        cancel: &cancel,
                        deadline: Some(Instant::now() + Duration::from_secs(10)),
                        stdin: None,
                        output_limit: 1024 * 1024,
                        on_line: None,
                    },
                )
            })?;
            match output {
                Ok(output) if output.code == Some(0) => match parse_probe(&output.stdout) {
                    Some(info) => {
                        if let Ok(mut cache) = PROBES.lock() {
                            cache.put(key, info.clone());
                        }
                        Ok(MultiValue::from_vec(vec![media_info_value(lua, &info)?]))
                    }
                    None => failure(lua, "FFprobe gave no readable answer"),
                },
                Ok(output) => failure(lua, output.stderr.trim()),
                Err(RunError::NotFound(_)) => failure(
                    lua,
                    "FFprobe isn't installed yet; it is downloaded when first needed",
                ),
                Err(error) => failure(lua, error),
            }
        })?,
    )?;
    Ok(table)
}

/// Extensions `app.system_open` refuses: they would run a program, which
/// needs a `process:` permission instead.
const RUNNABLE: &[&str] = &[
    "exe", "bat", "cmd", "com", "msi", "ps1", "vbs", "js", "jse", "wsf", "scr", "lnk", "sh", "app",
    "command",
];

fn app_table(lua: &Lua) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set(
        "open",
        lua.create_function(|_, path: String| {
            send(PluginMsg::App(AppRequest::Open(absolute(&path)?)));
            Ok(())
        })?,
    )?;
    table.set(
        "open_in_new_tab",
        lua.create_function(|_, path: String| {
            send(PluginMsg::App(AppRequest::OpenInNewTab(absolute(&path)?)));
            Ok(())
        })?,
    )?;
    table.set(
        "refresh",
        lua.create_function(|_, ()| {
            send(PluginMsg::App(AppRequest::Refresh));
            Ok(())
        })?,
    )?;
    table.set(
        "select",
        lua.create_function(|_, paths: Value| {
            send(PluginMsg::App(AppRequest::Select(paths_arg(paths)?)));
            Ok(())
        })?,
    )?;
    table.set(
        "system_open",
        lua.create_function(|lua, path: String| {
            require(lua, &Permission::FsRead)?;
            let path = absolute(&path)?;
            let ext = path
                .extension()
                .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if RUNNABLE.contains(&ext.as_str()) {
                return Err(mlua::Error::runtime(format!(
                    "system_open doesn't open programs ({}); use process.run",
                    path.display()
                )));
            }
            send(PluginMsg::App(AppRequest::SystemOpen(path)));
            Ok(())
        })?,
    )?;
    table.set(
        "open_terminal",
        lua.create_function(|_, path: String| {
            send(PluginMsg::App(AppRequest::OpenTerminal(absolute(&path)?)));
            Ok(())
        })?,
    )?;
    Ok(table)
}

fn clipboard_table(lua: &Lua) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set(
        "set_text",
        lua.create_function(|lua, text: String| {
            require(lua, &Permission::Clipboard)?;
            send(PluginMsg::App(AppRequest::Clipboard(text)));
            Ok(())
        })?,
    )?;
    Ok(table)
}

fn log_table(lua: &Lua) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    for level in ["info", "warn", "error"] {
        table.set(
            level,
            lua.create_function(move |lua, values: Variadic<Value>| {
                let text: Vec<String> = values.iter().map(super::sandbox::display_value).collect();
                let text = text.join(" ");
                let id = plugin_id(lua)?;
                match level {
                    "info" => log::info!("[{id}] {text}"),
                    "warn" => log::warn!("[{id}] {text}"),
                    _ => {
                        log::error!("[{id}] {text}");
                        send(PluginMsg::Error {
                            plugin: id,
                            message: text,
                        });
                    }
                }
                Ok(())
            })?,
        )?;
    }
    Ok(table)
}

fn job_table(lua: &Lua) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set(
        "progress",
        lua.create_function(|lua, (fraction, label): (f64, Option<String>)| {
            if let Some(job) = &services(lua)?.job {
                job.report(fraction as f32, label, false);
            }
            Ok(())
        })?,
    )?;
    table.set(
        "cancelled",
        lua.create_function(|lua, ()| Ok(services(lua)?.cancel.load(Ordering::Acquire)))?,
    )?;
    Ok(table)
}

fn storage_path(lua: &Lua) -> mlua::Result<PathBuf> {
    let id = plugin_id(lua)?;
    super::discovery::data_dir()
        .map(|dir| dir.join(format!("{id}.json")))
        .ok_or_else(|| mlua::Error::runtime("no data folder for plugin storage"))
}

fn read_storage(path: &Path) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn storage_table(lua: &Lua) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set(
        "get",
        lua.create_function(|lua, key: String| {
            let path = storage_path(lua)?;
            let storage = read_storage(&path);
            storage
                .get(&key)
                .map_or_else(|| Ok(Value::Nil), |value| lua.to_value(value))
        })?,
    )?;
    table.set(
        "set",
        lua.create_function(|lua, (key, value): (String, Value)| {
            let path = storage_path(lua)?;
            let mut storage = read_storage(&path);
            if value.is_nil() {
                storage.remove(&key);
            } else {
                storage.insert(key, lua.from_value(value)?);
            }
            let text = serde_json::to_string_pretty(&storage).map_err(mlua::Error::external)?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(mlua::Error::external)?;
            }
            std::fs::write(&path, text).map_err(mlua::Error::external)?;
            Ok(())
        })?,
    )?;
    Ok(table)
}

fn invalidate_table(lua: &Lua, bar: bool) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set(
        "invalidate",
        lua.create_function(move |lua, ()| {
            send(PluginMsg::Invalidate {
                plugin: plugin_id(lua)?,
                bar,
                inspector: !bar,
            });
            Ok(())
        })?,
    )?;
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::sandbox::tests::vm;

    fn run(vm: &crate::plugins::sandbox::Vm, code: &str) -> Result<Value, String> {
        vm.lua
            .load(code)
            .eval::<Value>()
            .map_err(|error| crate::plugins::sandbox::describe(&error))
    }

    #[test]
    fn permissions_are_enforced() {
        let vm = vm("return {}", &[]);
        let error = run(&vm, "return dirfleet.fs.exists('C:/')").expect_err("needs fs.read");
        assert!(error.contains("fs.read"), "{error}");
        let error =
            run(&vm, "return dirfleet.process.run{ cmd = 'git' }").expect_err("needs process");
        assert!(error.contains("process:git"), "{error}");
        let error = run(&vm, "return dirfleet.ffmpeg.available()").expect_err("needs ffmpeg");
        assert!(error.contains("ffmpeg"), "{error}");
        let error = run(&vm, "dirfleet.clipboard.set_text('x')").expect_err("needs clipboard");
        assert!(error.contains("clipboard"), "{error}");
    }

    #[test]
    fn path_helpers() {
        let vm = vm("return {}", &[]);
        let value = run(&vm, "return dirfleet.path.with_ext('/a/clip.mov', 'mp4')").expect("runs");
        assert_eq!(
            PathBuf::from(value.to_string().expect("string")),
            PathBuf::from("/a/clip.mp4")
        );
        let stem = run(&vm, "return dirfleet.path.stem('/a/clip.mov')").expect("runs");
        assert_eq!(stem.to_string().expect("string"), "clip");
        let duration = run(&vm, "return dirfleet.fmt.duration(3725)").expect("runs");
        assert_eq!(duration.to_string().expect("string"), "1:02:05");
    }

    #[test]
    fn unique_path_counts_up() {
        let dir = std::env::temp_dir().join(format!("dirfleet_unique_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let first = dir.join("clip.mp4");
        assert_eq!(unique_path(&first), first);
        std::fs::write(&first, b"x").expect("write");
        assert_eq!(unique_path(&first), dir.join("clip (2).mp4"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fs_reads_with_permission_and_rejects_relative_paths() {
        let vm = vm("return {}", &["fs.read"]);
        let dir = std::env::temp_dir();
        let code = format!("return dirfleet.fs.is_dir([[{}]])", dir.to_string_lossy());
        assert_eq!(run(&vm, &code).expect("runs"), Value::Boolean(true));
        let error = run(&vm, "return dirfleet.fs.exists('relative')").expect_err("relative");
        assert!(error.contains("absolute"), "{error}");
    }

    #[test]
    fn process_run_streams_lines() {
        let program = if cfg!(windows) { "cmd" } else { "sh" };
        let vm = vm("return {}", &[&format!("process:{program}")]);
        let script = if cfg!(windows) {
            "{ '/C', 'echo a&& echo b' }"
        } else {
            "{ '-c', 'echo a; echo b' }"
        };
        let code = format!(
            "local lines = {{}} local r = dirfleet.process.run{{ cmd = '{program}', args = {script}, on_line = function(l) lines[#lines+1] = l end }} return r.ok and table.concat(lines, ',')"
        );
        assert_eq!(
            run(&vm, &code).expect("runs").to_string().expect("string"),
            "a,b"
        );
    }

    #[test]
    fn parses_probe_and_encoders() {
        let info = parse_probe(
            r#"{"streams":[{"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"avg_frame_rate":"30000/1001","disposition":{"attached_pic":0}},{"codec_type":"audio","codec_name":"aac"}],"format":{"duration":"12.5","format_name":"mov,mp4"}}"#,
        )
        .expect("parses");
        assert_eq!(info.duration, Some(12.5));
        assert_eq!(info.width, Some(1920));
        assert_eq!(info.video_codec.as_deref(), Some("h264"));
        assert!(info.has_audio);
        assert!((info.fps.expect("fps") - 29.97).abs() < 0.01);
        let encoders = parse_encoders(
            "Encoders:\n V..... = Video\n ------\n V....D libx264              libx264 H.264\n A....D aac                  AAC\n",
        );
        assert_eq!(encoders, ["libx264", "aac"]);
        assert_eq!(progress_seconds("out_time_us=2500000"), Some(2.5));
        assert_eq!(progress_seconds("frame=10"), None);
    }

    #[test]
    fn probe_leaves_unknown_fields_nil() {
        let vm = vm("return {}", &[]);
        let info = MediaInfo {
            video_codec: Some("h264".into()),
            has_video: true,
            ..Default::default()
        };
        let info = media_info_value(&vm.lua, &info).expect("converts");
        vm.lua.globals().set("info", info).expect("sets global");
        let audio = run(&vm, "return info.audio_codec or 'None'").expect("runs");
        assert_eq!(
            audio
                .as_string()
                .and_then(|text| text.to_str().ok().map(|text| text.to_owned()))
                .as_deref(),
            Some("None")
        );
        let video = run(&vm, "return info.video_codec").expect("runs");
        assert_eq!(
            video
                .as_string()
                .and_then(|text| text.to_str().ok().map(|text| text.to_owned()))
                .as_deref(),
            Some("h264")
        );
    }
}
