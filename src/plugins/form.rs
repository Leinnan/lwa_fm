//! Forms a plugin asks for before an action runs.
//!
//! A form is data: a list of fields, built by Lua (`form(ctx)`) or listed in the
//! manifest. The shared crate checks it, draws it and validates the values, so no
//! Lua runs while the dialog is open.

pub use egui_mev_data::form::{FieldDef, Form, FormState, Outcome, Values};
