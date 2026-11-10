use std::path::PathBuf;

/// What a tab shows.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Default, Hash)]
pub enum CurrentPath {
    #[default]
    None,
    One(PathBuf),
    Multiple(Vec<PathBuf>),
    /// The Recycle Bin / Trash.
    Trash,
}

/// The Recycle Bin's name where it shows as a folder.
pub const TRASH_LABEL: &str = if cfg!(windows) {
    "Recycle Bin"
} else {
    "Trash"
};

impl CurrentPath {
    /// Every folder shown: one, several (favorites search) or none.
    pub fn get_paths(&self) -> Vec<PathBuf> {
        match self {
            Self::None | Self::Trash => Vec::new(),
            Self::One(path) => vec![path.clone()],
            Self::Multiple(paths) => paths.clone(),
        }
    }

    pub fn get_path(&self) -> Option<PathBuf> {
        match self {
            Self::None | Self::Trash => None,
            Self::One(path) => Some(path.clone()),
            Self::Multiple(paths) => {
                let mut common_path = paths.first()?.clone();
                for path in paths.iter().skip(1) {
                    let mut common_components = Vec::new();
                    let common_iter = common_path.components().zip(path.components());

                    for (a, b) in common_iter {
                        if a == b {
                            common_components.push(a);
                        } else {
                            break;
                        }
                    }

                    common_path = common_components.into_iter().collect();
                }
                Some(common_path)
            }
        }
    }

    pub fn get_name_from_path(&self) -> String {
        match &self {
            Self::None => "Empty".into(),
            Self::One(path) => {
                let name = path
                    .iter()
                    .next_back()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                // A drive root ("C:") reads better as "C:\".
                if cfg!(windows) && name.len() == 1 {
                    path.display().to_string()
                } else {
                    name
                }
            }
            Self::Multiple(_) => "Multiple".into(),
            Self::Trash => TRASH_LABEL.into(),
        }
    }

    pub fn parent(&self) -> Option<PathBuf> {
        match self {
            Self::One(path) => path.parent().map(PathBuf::from),
            _ => None,
        }
    }

    pub fn single_path(&self) -> Option<PathBuf> {
        match self {
            Self::One(path) => Some(path.clone()),
            _ => None,
        }
    }

    pub const fn multiple_paths(&self) -> bool {
        matches!(self, Self::Multiple(_))
    }

    /// Whether this is the Recycle Bin view, whose rows are not real files.
    pub const fn is_trash(&self) -> bool {
        matches!(self, Self::Trash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn several_folders_share_their_common_parent() {
        let path = CurrentPath::Multiple(vec!["/a/b/c".into(), "/a/b/d".into(), "/a/x".into()]);
        assert_eq!(path.get_path(), Some(PathBuf::from("/a")));
    }

    #[test]
    fn a_folder_is_named_after_its_last_component() {
        assert_eq!(
            CurrentPath::One("/a/Projects".into()).get_name_from_path(),
            "Projects"
        );
        assert_eq!(CurrentPath::Trash.get_name_from_path(), TRASH_LABEL);
    }
}
