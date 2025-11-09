use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(default)] // if we add new fields, give them default values when deserializing old state
#[derive(Default)]
pub struct DirectoryViewSettings {
    pub sorting: super::Sort,
    pub invert_sort: bool,
}

impl DirectoryViewSettings {
    pub fn change_sort(&mut self, new_sort: super::Sort) {
        if self.sorting == new_sort {
            self.invert_sort = !self.invert_sort;
        } else {
            self.sorting = new_sort;
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(default)] // if we add new fields, give them default values when deserializing old state
#[derive(Default)]
pub struct DirectoryShowHidden(pub bool);
