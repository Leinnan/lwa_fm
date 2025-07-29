use super::dir_handling::get_directories;
use std::{borrow::Cow, collections::BTreeSet, path::Path};

#[derive(Debug, Clone, Default)]
pub struct DirectoryPathInfo {
    pub text_input: String,
    pub possible_options: BTreeSet<Cow<'static, str>>,
}

impl DirectoryPathInfo {
    pub fn build(path: &Path, show_hidden: bool) -> Self {
        let possible_options = get_directories(path, show_hidden);
        Self {
            text_input: path.to_string_lossy().to_string(),
            possible_options,
        }
    }
}
