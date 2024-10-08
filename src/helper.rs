pub trait PathFixer {
    fn to_fixed_string(&self) -> String;
}

impl PathFixer for std::path::PathBuf {
    fn to_fixed_string(&self) -> String {
        self.display().to_string().replace("\\\\?\\", "")
    }
}
