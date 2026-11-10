//! Each enabled plugin's worker.
//!
//! A thread owning its Lua state, answering form, validate, quick action,
//! bar, inspector and event requests in order.
//! A newer bar or inspector request replaces one still waiting.

use super::Form;
pub use super::Request;
use super::permissions::PermissionSet;
use super::sandbox::{self, QUICK_BUDGET, Vm};
use super::{BarSegment, CallContext, InspectorRow, PluginCode, PluginMsg, send};
use mlua::{LuaSerdeExt as _, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

/// A running worker. Dropping it ends the thread.
pub struct Worker {
    sender: Sender<Request>,
    cancel: Arc<AtomicBool>,
    pub generation: u64,
}

impl Worker {
    pub fn spawn(code: Arc<PluginCode>, permissions: PermissionSet, generation: u64) -> Self {
        let (sender, receiver) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = Arc::clone(&cancel);
        let name = format!("plugin {}", code.id());
        let spawned = std::thread::Builder::new().name(name).spawn(move || {
            let plugin = code.id().to_owned();
            match sandbox::create(&code, permissions, thread_cancel, None) {
                Ok(vm) => {
                    send(PluginMsg::Loaded {
                        plugin,
                        generation,
                        result: Ok(()),
                    });
                    serve(&vm, &code, &receiver);
                }
                Err(error) => send(PluginMsg::Loaded {
                    plugin,
                    generation,
                    result: Err(error),
                }),
            }
        });
        if let Err(error) = spawned {
            log::error!("Could not start a plugin thread: {error}");
        }
        Self {
            sender,
            cancel,
            generation,
        }
    }

    pub fn request(&self, request: Request) {
        let _ = self.sender.send(request);
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Stops code still running; the closed channel ends the thread.
        self.cancel.store(true, Ordering::Release);
    }
}

fn serve(vm: &Vm, code: &PluginCode, receiver: &Receiver<Request>) {
    while let Ok(first) = receiver.recv() {
        let mut batch = vec![first];
        batch.extend(receiver.try_iter());
        for request in coalesce(batch) {
            handle(vm, code, request);
        }
    }
}

/// Drops bar requests for a folder asked about again, and all but the last
/// inspector request.
fn coalesce(batch: Vec<Request>) -> Vec<Request> {
    let mut out: Vec<Request> = Vec::with_capacity(batch.len());
    for request in batch {
        match &request {
            Request::Bar { dir, .. } => {
                out.retain(|other| !matches!(other, Request::Bar { dir: d, .. } if d == dir));
            }
            Request::Inspector { .. } => {
                out.retain(|other| !matches!(other, Request::Inspector { .. }));
            }
            _ => {}
        }
        out.push(request);
    }
    out
}

fn values_value(vm: &Vm, context: &CallContext) -> mlua::Result<Value> {
    match &context.values {
        Some(values) => vm.lua.to_value(values),
        None => Ok(Value::Table(vm.lua.create_table()?)),
    }
}

#[allow(clippy::too_many_lines)]
fn handle(vm: &Vm, code: &PluginCode, request: Request) {
    let plugin = code.id().to_owned();
    match request {
        Request::Form {
            request,
            action,
            context,
        } => {
            let result = (|| {
                let function = vm.handler(&["actions", &action, "form"]).ok_or_else(|| {
                    format!("form = \"dynamic\" needs a function M.actions.{action}.form")
                })?;
                let ctx = vm.context(&context).map_err(|e| e.to_string())?;
                let value: Value = vm.call(&function, ctx, Some(QUICK_BUDGET))?;
                let form: Form = vm
                    .lua
                    .from_value(value)
                    .map_err(|error| format!("form() returned a bad form: {error}"))?;
                form.check()?;
                Ok(form)
            })();
            send(PluginMsg::FormReady { request, result });
        }
        Request::Validate {
            request,
            action,
            context,
        } => {
            let result = (|| {
                let Some(function) = vm.handler(&["actions", &action, "validate"]) else {
                    return Ok(BTreeMap::new());
                };
                let ctx = vm.context(&context).map_err(|e| e.to_string())?;
                let values = values_value(vm, &context).map_err(|e| e.to_string())?;
                let value: Value = vm.call(&function, (ctx, values), Some(QUICK_BUDGET))?;
                match value {
                    Value::Nil | Value::Boolean(true) => Ok(BTreeMap::new()),
                    Value::String(text) => {
                        Ok(BTreeMap::from([(String::new(), text.to_string_lossy())]))
                    }
                    Value::Table(_) => vm
                        .lua
                        .from_value::<BTreeMap<String, String>>(value)
                        .map_err(|error| format!("validate() must return field errors: {error}")),
                    other => Err(format!(
                        "validate() must return nil or a table of errors, not a {}",
                        other.type_name()
                    )),
                }
            })();
            send(PluginMsg::Validated { request, result });
        }
        Request::Run { action, context } => {
            let result = (|| {
                let function = vm
                    .handler(&["actions", &action, "run"])
                    .ok_or_else(|| format!("no function M.actions.{action} (or .run)"))?;
                let ctx = vm.context(&context).map_err(|e| e.to_string())?;
                let values = values_value(vm, &context).map_err(|e| e.to_string())?;
                vm.call::<_, ()>(&function, (ctx, values), Some(QUICK_BUDGET))
            })();
            send(PluginMsg::ActionDone { plugin, result });
        }
        Request::Bar { dir, context } => {
            let handler = code
                .manifest
                .bar
                .as_ref()
                .map_or("bar", |bar| bar.handler.as_str());
            let result = (|| {
                let function = vm
                    .handler(&[handler])
                    .ok_or_else(|| format!("[bar] needs a function M.{handler}"))?;
                let ctx = vm.context(&context).map_err(|e| e.to_string())?;
                let value: Value = vm.call(&function, ctx, Some(QUICK_BUDGET))?;
                if value.is_nil() {
                    return Ok(Vec::new());
                }
                vm.lua
                    .from_value::<Vec<BarSegment>>(value)
                    .map_err(|error| format!("{handler}() returned bad segments: {error}"))
            })();
            send(PluginMsg::Bar {
                plugin,
                dir,
                result,
            });
        }
        Request::Inspector { key, context } => {
            let handler = code
                .manifest
                .inspector
                .as_ref()
                .map_or("inspector", |inspector| inspector.handler.as_str());
            let result = (|| {
                let function = vm
                    .handler(&[handler])
                    .ok_or_else(|| format!("[inspector] needs a function M.{handler}"))?;
                let ctx = vm.context(&context).map_err(|e| e.to_string())?;
                let value: Value = vm.call(&function, ctx, Some(QUICK_BUDGET))?;
                if value.is_nil() {
                    return Ok(Vec::new());
                }
                vm.lua
                    .from_value::<Vec<InspectorRow>>(value)
                    .map_err(|error| format!("{handler}() returned bad rows: {error}"))
            })();
            send(PluginMsg::Inspector {
                plugin,
                key,
                result,
            });
        }
        Request::Event {
            name,
            context,
            data,
        } => {
            let handler = format!("on_{name}");
            let Some(function) = vm.handler(&[&handler]) else {
                return;
            };
            let result = (|| {
                let ctx = vm.context(&context).map_err(|e| e.to_string())?;
                let data = vm.lua.to_value(&data).map_err(|e| e.to_string())?;
                vm.call::<_, ()>(&function, (ctx, data), Some(QUICK_BUDGET))
            })();
            if let Err(error) = result {
                send(PluginMsg::Error {
                    plugin,
                    message: format!("{handler}: {error}"),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalesces_bar_and_inspector_requests() {
        let bar = |dir: &str| Request::Bar {
            dir: dir.into(),
            context: CallContext::default(),
        };
        let inspector = |key| Request::Inspector {
            key,
            context: CallContext::default(),
        };
        let out = coalesce(vec![
            bar("a"),
            inspector(1),
            bar("b"),
            bar("a"),
            inspector(2),
            Request::Run {
                action: "x".into(),
                context: CallContext::default(),
            },
        ]);
        let shape: Vec<String> = out
            .iter()
            .map(|request| match request {
                Request::Bar { dir, .. } => format!("bar {}", dir.display()),
                Request::Inspector { key, .. } => format!("inspector {key}"),
                Request::Run { .. } => "run".into(),
                _ => "other".into(),
            })
            .collect();
        assert_eq!(shape, ["bar b", "bar a", "inspector 2", "run"]);
    }
}
