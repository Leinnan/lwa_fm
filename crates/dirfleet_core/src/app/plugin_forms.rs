//! A plugin action's form, as egui's: made by the plugin's Lua or listed in
//! its manifest, or a confirmation. The form's own rules (`egui_mev_data_core`)
//! check the values first, then the plugin's `validate`; values of a form
//! that says `remember` come back next time.
use std::collections::BTreeMap;

use crux_core::Command;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::plugins::{PluginControl, control};
use super::{DirFleet, Effect, Event, Model};
use crate::plugins::manifest::FormMode;
use crate::plugins::{CallContext, FieldDef, Form, Request, Values};
use egui_mev_data_core::form::{FieldType, PathMode, format_time, parse_time};

/// The open form.
pub(super) struct OpenForm {
    pub(super) plugin: String,
    pub(super) action: String,
    pub(super) title: String,
    pub(super) context: CallContext,
    pub(super) request: u64,
    /// `None` while the plugin makes it.
    pub(super) form: Option<Form>,
    pub(super) values: Values,
    pub(super) errors: BTreeMap<String, String>,
    pub(super) message: Option<String>,
    /// Why the form couldn't be made.
    pub(super) error: Option<String>,
    /// The plugin is checking the values.
    pub(super) busy: bool,
    /// What is typed into time fields, which may not read as a time yet.
    pub(super) time_text: BTreeMap<String, String>,
    /// The result row it was opened from, whose status follows the run.
    pub(super) row: Option<super::plugin_results::RowOrigin>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum FormFieldKind {
    Info,
    Paths,
    Separator,
    Group,
    Text,
    Multiline,
    Int,
    Float,
    Bool,
    Choice,
    Radio,
    Time,
    Path,
}

/// What a path field's Choose… picks.
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum FormPathMode {
    File,
    /// A file to write, which may not exist yet.
    Save,
    Folder,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FormOptionView {
    pub value: String,
    pub label: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FormPathView {
    pub path: String,
    pub size: String,
}

/// One visible field, in order; a group's fields follow it, one deeper.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FormFieldView {
    pub id: String,
    pub kind: FormFieldKind,
    pub label: String,
    /// An info field's text.
    pub text: String,
    pub help: String,
    pub placeholder: String,
    pub enabled: bool,
    /// How many groups it is in.
    pub depth: u32,
    /// The value as text: a number, a time, an option's value or text.
    pub value: String,
    /// A checkbox's value.
    pub on: bool,
    /// Bounds and step of numbers, in thousandths.
    pub min_milli: Option<i64>,
    pub max_milli: Option<i64>,
    pub step_milli: Option<i64>,
    pub slider: bool,
    pub options: Vec<FormOptionView>,
    pub paths: Vec<FormPathView>,
    pub path_mode: FormPathMode,
    /// Why the value can't be used, or "".
    pub error: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct PluginFormView {
    /// The tab whose window shows it.
    pub tab: u32,
    pub title: String,
    /// The plugin is making the form.
    pub loading: bool,
    /// Why the form couldn't be made, or "".
    pub error: String,
    pub fields: Vec<FormFieldView>,
    /// A message for the whole form, or "".
    pub message: String,
    pub submit: String,
    pub danger: bool,
    /// Only Close: the plugin says it can't run now.
    pub blocked: bool,
    /// The plugin is checking the values.
    pub busy: bool,
}

const fn kind_of(kind: FieldType) -> FormFieldKind {
    match kind {
        FieldType::Info => FormFieldKind::Info,
        FieldType::Paths => FormFieldKind::Paths,
        FieldType::Separator => FormFieldKind::Separator,
        FieldType::Group => FormFieldKind::Group,
        FieldType::Text => FormFieldKind::Text,
        FieldType::Multiline => FormFieldKind::Multiline,
        FieldType::Int => FormFieldKind::Int,
        FieldType::Float => FormFieldKind::Float,
        FieldType::Bool => FormFieldKind::Bool,
        FieldType::Choice => FormFieldKind::Choice,
        FieldType::Radio => FormFieldKind::Radio,
        FieldType::Time => FormFieldKind::Time,
        FieldType::Path => FormFieldKind::Path,
    }
}

#[allow(clippy::cast_possible_truncation)]
fn milli(number: Option<f64>) -> Option<i64> {
    number
        .filter(|number| number.is_finite())
        .map(|number| (number * 1000.0).round() as i64)
}

impl OpenForm {
    pub(super) fn new(
        plugin: String,
        action: String,
        title: String,
        context: CallContext,
        request: u64,
    ) -> Self {
        Self {
            plugin,
            action,
            title,
            context,
            request,
            form: None,
            values: Values::new(),
            errors: BTreeMap::new(),
            message: None,
            error: None,
            busy: false,
            time_text: BTreeMap::new(),
            row: None,
        }
    }

    /// Shows `form`, starting from what was remembered when it asks for it.
    pub(super) fn show(&mut self, form: Form, remembered: Option<&Values>) {
        self.values = form.initial_values(remembered);
        self.form = Some(form);
    }

    fn field(&self, id: &str) -> Option<&FieldDef> {
        self.form
            .as_ref()?
            .all_fields()
            .into_iter()
            .find(|field| field.kind.is_input() && field.id() == id)
    }

    /// What was typed into field `id`, as its type reads it.
    pub(super) fn set(&mut self, id: &str, text: String) {
        let Some(kind) = self.field(id).map(|field| field.kind) else {
            return;
        };
        let value = match kind {
            FieldType::Bool => Value::Bool(text == "true"),
            #[allow(clippy::cast_possible_truncation)]
            FieldType::Int => text
                .trim()
                .parse::<f64>()
                .map_or(Value::Null, |number| Value::from(number.round() as i64)),
            FieldType::Float => text
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|number| number.is_finite())
                .map_or(Value::Null, Value::from),
            FieldType::Time => {
                let value = parse_time(&text).map_or(Value::Null, Value::from);
                self.time_text.insert(id.to_owned(), text);
                value
            }
            _ => Value::String(text),
        };
        self.values.insert(id.to_owned(), value);
        self.errors.remove(id);
    }

    fn value_text(&self, field: &FieldDef) -> String {
        let value = self.values.get(field.id());
        match field.kind {
            FieldType::Time => self.time_text.get(field.id()).cloned().unwrap_or_else(|| {
                value
                    .and_then(Value::as_f64)
                    .map(format_time)
                    .unwrap_or_default()
            }),
            FieldType::Int | FieldType::Float => value.map(ToString::to_string).unwrap_or_default(),
            _ => value.and_then(Value::as_str).unwrap_or_default().to_owned(),
        }
    }

    pub(super) fn fields_view(
        &self,
        fields: &[FieldDef],
        enabled: bool,
        depth: u32,
        out: &mut Vec<FormFieldView>,
    ) {
        for field in fields {
            if !field.visible(&self.values) {
                continue;
            }
            let enabled = enabled && field.enabled(&self.values);
            out.push(FormFieldView {
                id: field.id().to_owned(),
                kind: kind_of(field.kind),
                label: field.label().to_owned(),
                text: field.text.clone().unwrap_or_default(),
                help: field.help.clone().unwrap_or_default(),
                placeholder: field.placeholder.clone().unwrap_or_default(),
                enabled,
                depth,
                value: self.value_text(field),
                on: self.values.get(field.id()).and_then(Value::as_bool) == Some(true),
                min_milli: milli(field.min),
                max_milli: milli(field.max),
                step_milli: milli(field.step),
                slider: field.slider,
                options: field
                    .options
                    .iter()
                    .map(|option| FormOptionView {
                        value: option.value().to_owned(),
                        label: option.label().to_owned(),
                    })
                    .collect(),
                paths: field
                    .rows
                    .iter()
                    .map(|row| FormPathView {
                        path: row.path.clone(),
                        size: row.size.clone().unwrap_or_default(),
                    })
                    .collect(),
                path_mode: match field.mode {
                    PathMode::File => FormPathMode::File,
                    PathMode::Save => FormPathMode::Save,
                    PathMode::Folder => FormPathMode::Folder,
                },
                error: self.errors.get(field.id()).cloned().unwrap_or_default(),
            });
            self.fields_view(&field.fields, enabled, depth + 1, out);
        }
    }

    pub(super) fn view(&self) -> PluginFormView {
        let mut fields = Vec::new();
        let (submit, danger) = self
            .form
            .as_ref()
            .map(|form| {
                self.fields_view(&form.fields, true, 0, &mut fields);
                form.submit_label(&self.values)
            })
            .unwrap_or_default();
        PluginFormView {
            tab: self.context.tab.unwrap_or_default(),
            title: self
                .form
                .as_ref()
                .and_then(|form| form.title.clone())
                .unwrap_or_else(|| self.title.clone()),
            loading: self.form.is_none() && self.error.is_none(),
            error: self.error.clone().unwrap_or_default(),
            fields,
            message: self.message.clone().unwrap_or_default(),
            submit,
            danger,
            blocked: self.form.as_ref().is_some_and(|form| form.blocked),
            busy: self.busy,
        }
    }
}

impl DirFleet {
    /// Opens the form of a plugin action that has one, as egui's: the
    /// plugin makes it, or the manifest lists it.
    pub(super) fn open_plugin_form(
        model: &mut Model,
        plugin: &str,
        action: &str,
        context: CallContext,
    ) -> Option<Command<Effect, Event>> {
        let def = model.plugins.action(plugin, action)?.clone();
        if def.form == FormMode::None {
            return None;
        }
        model.plugins.next_request += 1;
        let request = model.plugins.next_request;
        let mut open = OpenForm::new(
            plugin.to_owned(),
            action.to_owned(),
            def.title.clone(),
            context,
            request,
        );
        let command = if def.form == FormMode::Static {
            let form = Form {
                title: Some(def.title.clone()),
                remember: true,
                fields: def.fields,
                ..Default::default()
            };
            open.show(form, model.plugins.remembered(plugin, action));
            render()
        } else {
            let context = open.context.clone();
            render().and(control(PluginControl::Send {
                id: plugin.to_owned(),
                request: Request::Form {
                    request,
                    action: action.to_owned(),
                    context,
                },
            }))
        };
        model.plugins.form = Some(open);
        Some(command)
    }

    /// The form's events.
    pub(super) fn plugin_form_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        match event {
            Event::SetPluginFormField { id, text } => {
                if let Some(form) = model.plugins.form.as_mut() {
                    form.set(&id, text);
                }
                render()
            }
            Event::CancelPluginForm => {
                model.plugins.form = None;
                render()
            }
            Event::SubmitPluginForm => {
                let Some(open) = model.plugins.form.as_mut().filter(|open| !open.busy) else {
                    return Command::done();
                };
                let Some(form) = open.form.as_ref().filter(|form| !form.blocked) else {
                    return Command::done();
                };
                match form.validate(&open.values) {
                    Ok(values) => {
                        open.values = values;
                        open.errors.clear();
                        open.message = None;
                        open.busy = true;
                        let mut context = open.context.clone();
                        context.values = Some(open.values.clone());
                        render().and(control(PluginControl::Send {
                            id: open.plugin.clone(),
                            request: Request::Validate {
                                request: open.request,
                                action: open.action.clone(),
                                context,
                            },
                        }))
                    }
                    Err(errors) => {
                        open.errors = errors;
                        render()
                    }
                }
            }
            _ => Command::done(),
        }
    }

    /// The plugin made the form, or couldn't.
    pub(super) fn plugin_form_ready(
        model: &mut Model,
        request: u64,
        result: Result<Form, String>,
    ) -> Command<Effect, Event> {
        let Some(open) = model
            .plugins
            .form
            .as_ref()
            .filter(|open| open.request == request)
        else {
            return Command::done();
        };
        let remembered = model
            .plugins
            .remembered(&open.plugin, &open.action)
            .cloned();
        let open = model.plugins.form.as_mut().expect("checked above");
        match result {
            Ok(form) => open.show(form, remembered.as_ref()),
            Err(error) => open.error = Some(error),
        }
        render()
    }

    /// The plugin checked the values: the action runs, or the form says why
    /// not.
    pub(super) fn plugin_form_validated(
        model: &mut Model,
        request: u64,
        result: Result<BTreeMap<String, String>, String>,
    ) -> Command<Effect, Event> {
        let Some(open) = model
            .plugins
            .form
            .as_mut()
            .filter(|open| open.request == request)
        else {
            return Command::done();
        };
        open.busy = false;
        match result {
            Ok(errors) if errors.is_empty() => {
                let open = model.plugins.form.take().expect("checked above");
                if open.form.as_ref().is_some_and(|form| form.remember) {
                    model
                        .plugins
                        .settings
                        .entry(open.plugin.clone())
                        .or_default()
                        .remembered
                        .insert(open.action.clone(), open.values.clone());
                }
                let mut context = open.context;
                context.values = Some(open.values);
                let (command, job) =
                    Self::dispatch_plugin_action(model, &open.plugin, &open.action, context);
                if let (Some(job), Some(row)) = (job, open.row) {
                    model.plugins.row_jobs.push((job, row));
                }
                render().and(command)
            }
            Ok(mut errors) => {
                open.message = errors.remove("");
                open.errors = errors;
                render()
            }
            Err(error) => {
                open.message = Some(error);
                render()
            }
        }
    }
}
