use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug)]
#[serde(default)] // if we add new fields, give them default values when deserializing old state
#[derive(Default)]
pub struct DirectoryViewSettings {
    pub show_hidden: bool,
    pub sorting: super::Sort,
    pub invert_sort: bool,
}
