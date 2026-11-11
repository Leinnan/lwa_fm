//! What the command line asked for.
use std::path::PathBuf;

/// Launch options: `DirFleet [--state-file <file>] [--folder <folder>] [folder]`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Launch {
    /// Where state is kept between launches, instead of the usual place.
    pub state_file: Option<PathBuf>,
    /// The folder to show first.
    pub folder: Option<String>,
}

impl Launch {
    pub fn from_args(args: impl IntoIterator<Item = String>) -> Self {
        let mut launch = Self::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--state-file" => launch.state_file = args.next().map(PathBuf::from),
                "--folder" => launch.folder = args.next(),
                _ if launch.folder.is_none() && !arg.starts_with("--") => {
                    launch.folder = Some(arg);
                }
                _ => {}
            }
        }
        launch
    }

    /// The state file to use: the one asked for, or the app's own.
    pub fn state_file(&self) -> Option<PathBuf> {
        self.state_file.clone().or_else(|| {
            // Apart from the egui app's data, like the Mac app's.
            directories::ProjectDirs::from("io.github", "leinnan", "dirfleet.native")
                .map(|dirs| dirs.data_dir().join("state.json"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch(args: &[&str]) -> Launch {
        Launch::from_args(args.iter().map(ToString::to_string))
    }

    #[test]
    fn a_bare_argument_is_the_folder() {
        assert_eq!(launch(&["C:\\Users"]).folder.as_deref(), Some("C:\\Users"));
    }

    #[test]
    fn named_options_are_read() {
        let found = launch(&["--state-file", "s.json", "--folder", "D:\\"]);
        assert_eq!(found.state_file, Some(PathBuf::from("s.json")));
        assert_eq!(found.folder.as_deref(), Some("D:\\"));
    }

    #[test]
    fn unknown_options_are_skipped() {
        assert_eq!(launch(&["--what", "D:\\"]).folder.as_deref(), Some("D:\\"));
    }
}
