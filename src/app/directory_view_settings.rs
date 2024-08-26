use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug)]
#[serde(default)] // if we add new fields, give them default values when deserializing old state
pub struct DirectoryViewSettings {
    pub show_hidden: bool,
    pub sorting: super::Sort,
    pub invert_sort: bool,
    #[serde(skip)]
    pub search: super::Search,
}

impl Default for DirectoryViewSettings {
    fn default() -> Self {
        Self {
            show_hidden: false,
            sorting: super::Sort::default(),
            invert_sort: false,
            search: super::Search {
                visible: false,
                case_sensitive: false,
                depth: 3,
                favorites: false,
                value: String::new(),
            },
        }
    }
}

impl DirectoryViewSettings {
    pub fn is_searching(&self) -> bool {
        !self.search.value.is_empty()
    }
}
