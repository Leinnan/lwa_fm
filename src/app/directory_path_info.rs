use super::dir_handling::get_directories;
use crate::helper::PathFixer;
use std::{collections::BTreeSet, path::Path};

#[derive(Debug, Clone, Default)]
pub struct DirectoryPathInfo {
    pub editable: bool,
    pub top_edit: String,
    pub possible_options: BTreeSet<String>,
}

impl DirectoryPathInfo {
    pub fn rebuild(&mut self, path: &Path, show_hidden: bool) {
        self.top_edit = path.to_path_buf().to_fixed_string();
        self.possible_options = get_directories(path, show_hidden);
    }
}
