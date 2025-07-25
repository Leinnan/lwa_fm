use super::dir_handling::get_directories;
use crate::{app::dock::CurrentPath, helper::PathFixer};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Default)]
pub struct DirectoryPathInfo {
    pub editable: bool,
    pub top_edit: String,
    pub possible_options: BTreeSet<String>,
}

impl DirectoryPathInfo {
    pub fn rebuild(&mut self, path: &CurrentPath, show_hidden: bool) {
        if let CurrentPath::One(path_buf) = path {
            self.top_edit = path_buf.to_fixed_string();
            self.possible_options = get_directories(path_buf, show_hidden);
        } else {
            self.top_edit = String::new();
            self.possible_options = BTreeSet::new();
        }
    }
}
