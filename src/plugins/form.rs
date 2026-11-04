//! Forms a plugin asks for before an action runs.
//!
//! A form is data: a list of fields, built by Lua (`form(ctx)`) or listed in
//! the manifest. The host checks it, draws it with egui and validates the
//! values, so no Lua runs while the dialog is open. `visible_if` and
//! `enabled_if` are evaluated here for the same reason.

use crate::app::ui::{ButtonKind, DialogButton};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Submitted values by field id: strings, numbers and booleans.
pub type Values = Map<String, Value>;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Form {
    pub title: Option<String>,
    /// The submit button's text.
    pub submit: Option<String>,
    /// The submit button removes something for good: draw it red.
    #[serde(default)]
    pub danger: bool,
    /// Other looks for the submit button, depending on the values.
    #[serde(default)]
    pub submit_when: Vec<SubmitVariant>,
    /// There is nothing to submit, as the fields explain: only Close remains.
    #[serde(default)]
    pub blocked: bool,
    /// Start from the values used last time.
    #[serde(default)]
    pub remember: bool,
    #[serde(default)]
    pub fields: Vec<FieldDef>,
}

/// The submit button while a condition holds; the first that does wins.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitVariant {
    pub when: Condition,
    pub label: String,
    #[serde(default)]
    pub danger: bool,
}

/// One line of a `paths` field.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathRow {
    pub path: String,
    pub size: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    /// Read-only text.
    Info,
    /// Folders or files an action will affect, with their sizes, in a card.
    Paths,
    Separator,
    /// A labelled box around `fields`.
    Group,
    Text,
    Multiline,
    Int,
    Float,
    Bool,
    /// A drop-down.
    Choice,
    Radio,
    /// A duration or position in seconds, typed as `hh:mm:ss.mmm`.
    Time,
    Path,
}

impl FieldType {
    /// Whether the field holds a value (and so needs an id).
    pub const fn is_input(self) -> bool {
        !matches!(
            self,
            Self::Info | Self::Paths | Self::Separator | Self::Group
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ChoiceOption {
    /// `{"value", "Label"}`
    Pair(String, String),
    /// `{ value = "v", label = "Label" }`
    Named { value: String, label: String },
    /// `"value"`, which is also its label.
    Plain(String),
}

impl ChoiceOption {
    pub fn value(&self) -> &str {
        match self {
            Self::Pair(value, _) | Self::Named { value, .. } | Self::Plain(value) => value,
        }
    }
    pub fn label(&self) -> &str {
        match self {
            Self::Pair(_, label) | Self::Named { label, .. } => label,
            Self::Plain(value) => value,
        }
    }
}

/// `visible_if` / `enabled_if`: compares another field's value.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    pub field: String,
    pub equals: Option<Value>,
    pub not_equals: Option<Value>,
    #[serde(default, rename = "in")]
    pub one_of: Vec<Value>,
    #[serde(default)]
    pub not_in: Vec<Value>,
}

impl Condition {
    fn holds(&self, values: &Values) -> bool {
        let value = values.get(&self.field).unwrap_or(&Value::Null);
        self.equals
            .as_ref()
            .is_none_or(|expected| same(value, expected))
            && self
                .not_equals
                .as_ref()
                .is_none_or(|other| !same(value, other))
            && (self.one_of.is_empty() || self.one_of.iter().any(|other| same(value, other)))
            && !self.not_in.iter().any(|other| same(value, other))
    }
}

/// Equality where `1` and `1.0` are the same number.
fn same(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(a), Some(b)) => (a - b).abs() < f64::EPSILON,
        _ => a == b,
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathMode {
    #[default]
    File,
    Save,
    Folder,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldDef {
    #[serde(rename = "type")]
    pub kind: FieldType,
    pub id: Option<String>,
    pub label: Option<String>,
    /// For `info`: the text shown.
    pub text: Option<String>,
    pub default: Option<Value>,
    #[serde(default)]
    pub required: bool,
    /// Shown when hovering the field.
    pub help: Option<String>,
    pub placeholder: Option<String>,
    pub enabled: Option<bool>,
    pub visible_if: Option<Condition>,
    pub enabled_if: Option<Condition>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    #[serde(default)]
    pub slider: bool,
    #[serde(default)]
    pub options: Vec<ChoiceOption>,
    #[serde(default)]
    pub fields: Vec<Self>,
    #[serde(default)]
    pub mode: PathMode,
    /// For `paths`.
    #[serde(default)]
    pub rows: Vec<PathRow>,
}

impl FieldDef {
    fn id(&self) -> &str {
        self.id.as_deref().unwrap_or_default()
    }

    fn label(&self) -> &str {
        self.label
            .as_deref()
            .or(self.id.as_deref())
            .unwrap_or_default()
    }

    pub fn visible(&self, values: &Values) -> bool {
        self.visible_if
            .as_ref()
            .is_none_or(|condition| condition.holds(values))
    }

    pub fn enabled(&self, values: &Values) -> bool {
        self.enabled.unwrap_or(true)
            && self
                .enabled_if
                .as_ref()
                .is_none_or(|condition| condition.holds(values))
    }

    /// The value a field starts with when there is no default.
    fn empty_value(&self) -> Value {
        match self.kind {
            FieldType::Bool => Value::Bool(false),
            FieldType::Int => Value::from(self.min.map_or(0, |min| min.ceil() as i64).max(0)),
            FieldType::Float | FieldType::Time => Value::from(self.min.unwrap_or(0.0).max(0.0)),
            FieldType::Choice | FieldType::Radio => self
                .options
                .first()
                .map_or(Value::Null, |option| Value::from(option.value())),
            FieldType::Text | FieldType::Multiline | FieldType::Path => Value::from(""),
            FieldType::Info | FieldType::Paths | FieldType::Separator | FieldType::Group => {
                Value::Null
            }
        }
    }

    /// `value` converted to this field's type and range, or why it can't be.
    fn conform(&self, value: &Value) -> Result<Value, String> {
        match self.kind {
            FieldType::Bool => value
                .as_bool()
                .map(Value::Bool)
                .ok_or_else(|| "must be true or false".into()),
            FieldType::Int => {
                let number = value.as_f64().ok_or("must be a whole number")?;
                let clamped = self.clamp(number.round());
                Ok(Value::from(clamped as i64))
            }
            FieldType::Float | FieldType::Time => {
                let number = value
                    .as_f64()
                    .filter(|n| n.is_finite())
                    .ok_or("must be a number")?;
                Ok(Value::from(self.clamp(number)))
            }
            FieldType::Choice | FieldType::Radio => {
                let text = match value {
                    Value::String(text) => text.clone(),
                    Value::Number(number) => number.to_string(),
                    _ => return Err("must be one of the options".into()),
                };
                if self.options.iter().any(|option| option.value() == text) {
                    Ok(Value::String(text))
                } else {
                    Err(format!("\"{text}\" is not one of the options"))
                }
            }
            FieldType::Text | FieldType::Multiline | FieldType::Path => match value {
                Value::String(text) => Ok(Value::String(text.clone())),
                Value::Number(number) => Ok(Value::String(number.to_string())),
                _ => Err("must be text".into()),
            },
            FieldType::Info | FieldType::Paths | FieldType::Separator | FieldType::Group => {
                Ok(Value::Null)
            }
        }
    }

    fn clamp(&self, number: f64) -> f64 {
        let number = self.min.map_or(number, |min| number.max(min));
        self.max.map_or(number, |max| number.min(max))
    }
}

/// Every field, groups flattened, in order.
fn walk<'a>(fields: &'a [FieldDef], out: &mut Vec<&'a FieldDef>) {
    for field in fields {
        out.push(field);
        walk(&field.fields, out);
    }
}

impl Form {
    pub fn all_fields(&self) -> Vec<&FieldDef> {
        let mut out = Vec::new();
        walk(&self.fields, &mut out);
        out
    }

    /// Checks the schema itself: ids, options, ranges and defaults.
    pub fn check(&self) -> Result<(), String> {
        let fields = self.all_fields();
        if fields.len() > 128 {
            return Err("a form can have at most 128 fields".into());
        }
        let mut ids: Vec<&str> = Vec::new();
        for field in &fields {
            if field.kind.is_input() {
                let id = field
                    .id
                    .as_deref()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| {
                        format!("a {:?} field needs an id", field.kind).to_lowercase()
                    })?;
                if ids.contains(&id) {
                    return Err(format!("field id \"{id}\" is used twice"));
                }
                ids.push(id);
            }
            if field.kind == FieldType::Group && field.label.is_none() {
                return Err("a group needs a label".into());
            }
            if field.kind != FieldType::Group && !field.fields.is_empty() {
                return Err(format!("field \"{}\": only groups have fields", field.id()));
            }
            if matches!(field.kind, FieldType::Choice | FieldType::Radio)
                && field.options.is_empty()
            {
                return Err(format!("field \"{}\" needs options", field.id()));
            }
            if let (Some(min), Some(max)) = (field.min, field.max)
                && min > max
            {
                return Err(format!("field \"{}\": min is above max", field.id()));
            }
            if let Some(default) = &field.default
                && field.kind.is_input()
            {
                field
                    .conform(default)
                    .map_err(|error| format!("field \"{}\" default {error}", field.id()))?;
            }
        }
        for field in &fields {
            for condition in [&field.visible_if, &field.enabled_if].into_iter().flatten() {
                if !ids.contains(&condition.field.as_str()) {
                    return Err(format!(
                        "field \"{}\" depends on unknown field \"{}\"",
                        field.id(),
                        condition.field
                    ));
                }
            }
        }
        Ok(())
    }

    /// Starting values: the defaults, then values remembered from last time
    /// where they still fit the field.
    pub fn initial_values(&self, remembered: Option<&Values>) -> Values {
        let mut values = Values::new();
        for field in self.all_fields() {
            if !field.kind.is_input() {
                continue;
            }
            let default = field
                .default
                .as_ref()
                .and_then(|value| field.conform(value).ok())
                .unwrap_or_else(|| field.empty_value());
            // A field the form disabled this time keeps its default.
            let value = remembered
                .filter(|_| self.remember && field.enabled != Some(false))
                .and_then(|values| values.get(field.id()))
                .and_then(|value| field.conform(value).ok())
                .unwrap_or(default);
            values.insert(field.id().to_owned(), value);
        }
        values
    }

    /// The values to submit, or an error per field. Hidden and disabled
    /// fields keep their value but aren't required.
    pub fn validate(&self, values: &Values) -> Result<Values, BTreeMap<String, String>> {
        let mut out = Values::new();
        let mut errors = BTreeMap::new();
        for field in self.all_fields() {
            if !field.kind.is_input() {
                continue;
            }
            let id = field.id().to_owned();
            let value = values
                .get(&id)
                .cloned()
                .unwrap_or_else(|| field.empty_value());
            match field.conform(&value) {
                Ok(value) => {
                    let active = field.visible(values) && field.enabled(values);
                    if active
                        && field.required
                        && value.as_str().is_some_and(|s| s.trim().is_empty())
                    {
                        errors.insert(id.clone(), "Required".into());
                    }
                    out.insert(id, value);
                }
                Err(error) => {
                    errors.insert(id, error);
                }
            }
        }
        if errors.is_empty() {
            Ok(out)
        } else {
            Err(errors)
        }
    }
}

/// Formats seconds as `h:mm:ss.mmm` (hours only when needed).
pub fn format_time(seconds: f64) -> String {
    let millis = (seconds.max(0.0) * 1000.0).round() as u64;
    let (hours, rest) = (millis / 3_600_000, millis % 3_600_000);
    let (minutes, rest) = (rest / 60_000, rest % 60_000);
    let (secs, millis) = (rest / 1000, rest % 1000);
    let base = if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes:02}:{secs:02}")
    };
    if millis == 0 {
        base
    } else {
        format!("{base}.{millis:03}")
    }
}

/// Parses `ss`, `mm:ss`, `hh:mm:ss`, each with optional `.fraction`.
pub fn parse_time(text: &str) -> Option<f64> {
    let parts: Vec<&str> = text.trim().split(':').collect();
    if parts.is_empty() || parts.len() > 3 || parts.iter().any(|part| part.is_empty()) {
        return None;
    }
    let mut seconds: f64 = 0.0;
    for (index, part) in parts.iter().enumerate() {
        let last = index + 1 == parts.len();
        let value: f64 = if last {
            part.parse().ok()?
        } else {
            f64::from(part.parse::<u32>().ok()?)
        };
        if !value.is_finite() || value < 0.0 || (index > 0 && value >= 60.0) {
            return None;
        }
        seconds = seconds.mul_add(60.0, value);
    }
    Some(seconds)
}

/// What happened in the dialog this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Open,
    Submit,
    Cancel,
}

/// An open form dialog.
#[derive(Debug, Clone)]
pub struct FormState {
    pub form: Form,
    pub values: Values,
    pub errors: BTreeMap<String, String>,
    /// An error for the whole form, from `validate` or a failed call.
    pub message: Option<String>,
    /// Text being typed into time fields, by id.
    time_text: BTreeMap<String, String>,
    /// Waiting for the plugin's `validate`.
    pub busy: bool,
}

/// The `paths` field: each path on the left, its size lined up on the right.
fn paths_ui(ui: &mut egui::Ui, rows: &[PathRow]) {
    crate::app::ui::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 4.0;
        let height = ui.spacing().interact_size.y;
        for row in rows {
            // A set height: a right-to-left layout would take all there is.
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), height),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    if let Some(size) = &row.size {
                        ui.strong(size);
                    }
                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), height),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.add(egui::Label::new(row.path.as_str()).truncate())
                                .on_hover_text(row.path.as_str());
                        },
                    );
                },
            );
        }
    });
}

impl FormState {
    pub fn new(form: Form, remembered: Option<&Values>) -> Self {
        let values = form.initial_values(remembered);
        Self {
            form,
            values,
            errors: BTreeMap::new(),
            message: None,
            time_text: BTreeMap::new(),
            busy: false,
        }
    }

    /// Draws the fields in one row, for a toolbar: radio fields as segmented
    /// buttons, which need no label of their own.
    pub fn toolbar_ui(&mut self, ui: &mut egui::Ui) {
        let fields = std::mem::take(&mut self.form.fields);
        for field in &fields {
            if !field.visible(&self.values) {
                continue;
            }
            if field.kind == FieldType::Radio {
                let current = self
                    .values
                    .get(field.id())
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let selected = field
                    .options
                    .iter()
                    .position(|option| option.value() == current)
                    .unwrap_or(0);
                let tip = field.help.as_deref().unwrap_or_default();
                let choices: Vec<(&str, bool, &str)> = field
                    .options
                    .iter()
                    .map(|option| (option.label(), field.enabled(&self.values), tip))
                    .collect();
                if let Some(index) = crate::app::ui::segmented_text(ui, &choices, selected) {
                    self.values.insert(
                        field.id().to_owned(),
                        Value::from(field.options[index].value()),
                    );
                }
            } else if field.kind.is_input() {
                ui.label(field.label());
                self.input_ui(ui, field);
            }
        }
        self.form.fields = fields;
    }

    /// What the last check or run had to say, if anything.
    pub fn message_ui(&self, ui: &mut egui::Ui) {
        if let Some(message) = &self.message {
            ui.colored_label(ui.visuals().error_fg_color, message);
        }
    }

    /// The submit button's text and kind for the values now.
    fn submit_button(&self) -> (String, ButtonKind) {
        let kind = |danger: bool| {
            if danger {
                ButtonKind::Danger
            } else {
                ButtonKind::Primary
            }
        };
        self.form
            .submit_when
            .iter()
            .find(|variant| variant.when.holds(&self.values))
            .map_or_else(
                || {
                    (
                        self.form.submit.clone().unwrap_or_else(|| "Run".into()),
                        kind(self.form.danger),
                    )
                },
                |variant| (variant.label.clone(), kind(variant.danger)),
            )
    }

    /// Draws the dialog's fields and buttons.
    pub fn ui(&mut self, ui: &mut egui::Ui, default_title: &str) -> Outcome {
        let mut outcome = Outcome::Open;
        let title = self
            .form
            .title
            .clone()
            .unwrap_or_else(|| default_title.to_owned());
        ui.heading(title);
        ui.add_space(8.0);
        let fields = std::mem::take(&mut self.form.fields);
        egui::ScrollArea::vertical()
            .auto_shrink([false, true])
            .max_height((ui.ctx().content_rect().height() - 200.0).max(160.0))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                self.fields_ui(ui, &fields);
            });
        self.form.fields = fields;
        self.message_ui(ui);
        if self.busy {
            ui.spinner();
        }
        let (label, kind) = self.submit_button();
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift)
            && !ui.ctx().text_edit_focused();
        let submit = crate::app::ui::DialogButton {
            label: &label,
            kind,
            enabled: !self.busy,
        };
        if self.form.blocked {
            let close = [DialogButton::plain("Close")];
            if crate::app::ui::dialog_buttons(ui, false, &close).is_some() {
                outcome = Outcome::Cancel;
            }
        } else {
            let buttons = [submit, DialogButton::plain("Cancel")];
            match crate::app::ui::dialog_buttons(ui, false, &buttons) {
                Some(0) => outcome = Outcome::Submit,
                Some(_) => outcome = Outcome::Cancel,
                None if enter && !self.busy => outcome = Outcome::Submit,
                None => {}
            }
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            outcome = Outcome::Cancel;
        }
        outcome
    }

    /// Text and lists take the dialog's whole width; runs of inputs between
    /// them line up in a grid of their own.
    fn fields_ui(&mut self, ui: &mut egui::Ui, fields: &[FieldDef]) {
        let visible: Vec<&FieldDef> = fields
            .iter()
            .filter(|field| field.visible(&self.values))
            .collect();
        let mut at = 0;
        while at < visible.len() {
            let field = visible[at];
            match field.kind {
                FieldType::Info => {
                    if let Some(text) = field.text.as_deref().filter(|text| !text.is_empty()) {
                        ui.add(egui::Label::new(text).wrap());
                    }
                }
                FieldType::Paths => paths_ui(ui, &field.rows),
                FieldType::Separator => {
                    ui.separator();
                }
                FieldType::Group => {
                    ui.label(egui::RichText::new(field.label()).strong());
                    self.fields_ui(ui, &field.fields);
                }
                _ => {
                    let start = at;
                    while visible.get(at + 1).is_some_and(|next| next.kind.is_input()) {
                        at += 1;
                    }
                    egui::Grid::new(("plugin_form", field.id()))
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            for field in &visible[start..=at] {
                                self.input_row_ui(ui, field);
                            }
                        });
                }
            }
            at += 1;
        }
    }

    /// A row of the grid: the field's label, then its input and any error.
    fn input_row_ui(&mut self, ui: &mut egui::Ui, field: &FieldDef) {
        let label = ui.label(field.label());
        if let Some(help) = &field.help {
            label.on_hover_text(help);
        }
        let enabled = field.enabled(&self.values);
        ui.add_enabled_ui(enabled, |ui| {
            ui.vertical(|ui| {
                self.input_ui(ui, field);
                if let Some(error) = self.errors.get(field.id()) {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
            });
        });
        ui.end_row();
    }

    #[allow(clippy::too_many_lines)]
    fn input_ui(&mut self, ui: &mut egui::Ui, field: &FieldDef) {
        let id = field.id().to_owned();
        let value = self.values.get(&id).cloned().unwrap_or(Value::Null);
        let changed = match field.kind {
            FieldType::Text | FieldType::Path => {
                let mut text = value.as_str().unwrap_or_default().to_owned();
                let mut edit = crate::app::ui::text_field(ui, &mut text).desired_width(260.0);
                if let Some(hint) = &field.placeholder {
                    edit = edit.hint_text(hint.as_str());
                }
                let changed = ui.add(edit).changed();
                changed.then_some(Value::String(text))
            }
            FieldType::Multiline => {
                let mut text = value.as_str().unwrap_or_default().to_owned();
                let mut edit = egui::TextEdit::multiline(&mut text)
                    .margin(crate::app::ui::text_field_margin(ui))
                    .desired_width(260.0)
                    .desired_rows(4);
                if let Some(hint) = &field.placeholder {
                    edit = edit.hint_text(hint.as_str());
                }
                ui.add(edit).changed().then_some(Value::String(text))
            }
            FieldType::Bool => {
                let mut on = value.as_bool().unwrap_or(false);
                ui.checkbox(&mut on, "")
                    .changed()
                    .then_some(Value::Bool(on))
            }
            FieldType::Int => {
                let mut number = value.as_i64().unwrap_or(0);
                let min = field.min.map_or(i64::MIN, |min| min.ceil() as i64);
                let max = field.max.map_or(i64::MAX, |max| max.floor() as i64);
                let changed = if field.slider && field.min.is_some() && field.max.is_some() {
                    ui.add(egui::Slider::new(&mut number, min..=max)).changed()
                } else {
                    ui.add(
                        egui::DragValue::new(&mut number)
                            .range(min..=max)
                            .speed(field.step.unwrap_or(1.0)),
                    )
                    .changed()
                };
                changed.then(|| Value::from(number))
            }
            FieldType::Float => {
                let mut number = value.as_f64().unwrap_or(0.0);
                let min = field.min.unwrap_or(f64::MIN);
                let max = field.max.unwrap_or(f64::MAX);
                let changed = if field.slider && field.min.is_some() && field.max.is_some() {
                    ui.add(egui::Slider::new(&mut number, min..=max)).changed()
                } else {
                    ui.add(
                        egui::DragValue::new(&mut number)
                            .range(min..=max)
                            .speed(field.step.unwrap_or(0.1)),
                    )
                    .changed()
                };
                changed.then(|| Value::from(number))
            }
            FieldType::Choice => {
                let current = value.as_str().unwrap_or_default().to_owned();
                let selected = field
                    .options
                    .iter()
                    .find(|option| option.value() == current)
                    .map_or(current.as_str(), ChoiceOption::label)
                    .to_owned();
                let mut picked = None;
                egui::ComboBox::from_id_salt(("plugin_form_choice", &id))
                    .selected_text(selected)
                    .width(200.0)
                    .show_ui(ui, |ui| {
                        for option in &field.options {
                            if ui
                                .selectable_label(option.value() == current, option.label())
                                .clicked()
                            {
                                picked = Some(Value::from(option.value()));
                            }
                        }
                    });
                picked
            }
            FieldType::Radio => {
                let current = value.as_str().unwrap_or_default().to_owned();
                let mut picked = None;
                ui.horizontal_wrapped(|ui| {
                    for option in &field.options {
                        if ui
                            .radio(option.value() == current, option.label())
                            .clicked()
                        {
                            picked = Some(Value::from(option.value()));
                        }
                    }
                });
                picked
            }
            FieldType::Time => {
                let seconds = value.as_f64().unwrap_or(0.0);
                let text = self
                    .time_text
                    .entry(id.clone())
                    .or_insert_with(|| format_time(seconds));
                let response = ui.add(
                    crate::app::ui::text_field(ui, text)
                        .desired_width(120.0)
                        .hint_text("hh:mm:ss.mmm"),
                );
                let mut result = None;
                if response.changed() {
                    match parse_time(text) {
                        Some(parsed) => {
                            self.errors.remove(&id);
                            result = Some(Value::from(parsed));
                        }
                        None => {
                            self.errors
                                .insert(id.clone(), "Use hh:mm:ss.mmm or seconds".into());
                        }
                    }
                }
                if response.lost_focus() && parse_time(text).is_some() {
                    *text = format_time(field.clamp(parse_time(text).unwrap_or(seconds)));
                }
                if let Some(max) = field.max {
                    ui.weak(format!("of {}", format_time(max)));
                }
                result
            }
            FieldType::Info | FieldType::Paths | FieldType::Separator | FieldType::Group => None,
        };
        if let Some(value) = changed {
            if field.kind != FieldType::Time {
                self.errors.remove(&id);
            }
            self.values.insert(id, value);
            self.message = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn form(value: Value) -> Form {
        serde_json::from_value(value).expect("form")
    }

    fn convert_form() -> Form {
        form(json!({
            "remember": true,
            "fields": [
                { "type": "info", "text": "1920x1080" },
                { "id": "format", "type": "choice", "options": [["mp4", "MP4"], ["webm", "WebM"]], "default": "mp4" },
                { "id": "range", "type": "radio", "options": [["all", "Whole"], ["part", "Fragment"]], "default": "all" },
                { "id": "start", "type": "time", "default": 0, "max": 60, "visible_if": { "field": "range", "equals": "part" } },
                { "id": "crf", "type": "int", "min": 0, "max": 51, "default": 23 },
                { "id": "name", "type": "text", "required": true, "visible_if": { "field": "format", "not_in": ["webm"] } }
            ]
        }))
    }

    #[test]
    fn checks_schema() {
        convert_form().check().expect("valid");
        let missing_id = form(json!({ "fields": [{ "type": "text" }] }));
        assert!(missing_id.check().is_err());
        let bad_default = form(
            json!({ "fields": [{ "id": "a", "type": "choice", "options": ["x"], "default": "y" }] }),
        );
        assert!(bad_default.check().is_err());
        let unknown_dep = form(
            json!({ "fields": [{ "id": "a", "type": "bool", "visible_if": { "field": "b", "equals": true } }] }),
        );
        assert!(unknown_dep.check().is_err());
        assert!(
            serde_json::from_value::<Form>(json!({ "fields": [{ "type": "text", "colour": 1 }] }))
                .is_err()
        );
    }

    #[test]
    fn initial_values_use_defaults_and_matching_memory() {
        let form = convert_form();
        let values = form.initial_values(None);
        assert_eq!(values["format"], json!("mp4"));
        assert_eq!(values["crf"], json!(23));
        assert_eq!(values["name"], json!(""));
        let mut remembered = Values::new();
        remembered.insert("format".into(), json!("webm"));
        remembered.insert("crf".into(), json!(99));
        remembered.insert("gone".into(), json!(1));
        let values = form.initial_values(Some(&remembered));
        assert_eq!(values["format"], json!("webm"));
        assert_eq!(values["crf"], json!(51), "clamped to max");
        assert!(!values.contains_key("gone"));
    }

    #[test]
    fn visibility_and_required() {
        let form = convert_form();
        let mut values = form.initial_values(None);
        let start = &form.all_fields()[3];
        assert!(!start.visible(&values));
        values.insert("range".into(), json!("part"));
        assert!(start.visible(&values));
        let errors = form.validate(&values).expect_err("name required");
        assert_eq!(errors["name"], "Required");
        values.insert("format".into(), json!("webm"));
        let ok = form.validate(&values).expect("hidden field isn't required");
        assert_eq!(ok["range"], json!("part"));
    }

    #[test]
    fn times_round_trip() {
        assert_eq!(parse_time("90"), Some(90.0));
        assert_eq!(parse_time("1:30"), Some(90.0));
        assert_eq!(parse_time("1:00:00.5"), Some(3600.5));
        assert_eq!(parse_time("1:60"), None);
        assert_eq!(parse_time("x"), None);
        assert_eq!(format_time(3600.5), "1:00:00.500");
        assert_eq!(format_time(75.0), "01:15");
    }
}
