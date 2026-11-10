//! What a plugin may do. The manifest asks; the user approves when enabling
//! the plugin, and every host function checks the approved set.

use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Permission {
    /// Read files and folders.
    FsRead,
    /// Create, change, move and delete files.
    FsWrite,
    /// Run this program (a name like `git`, looked up on `PATH`).
    Process(String),
    /// Run the `FFmpeg` and `FFprobe` the app uses for previews.
    Ffmpeg,
    /// Put text on the clipboard.
    Clipboard,
}

impl Permission {
    /// The permission `text` names.
    ///
    /// # Errors
    /// When it names none.
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "fs.read" => Ok(Self::FsRead),
            "fs.write" => Ok(Self::FsWrite),
            "ffmpeg" => Ok(Self::Ffmpeg),
            "clipboard" => Ok(Self::Clipboard),
            _ => match text.strip_prefix("process:") {
                Some(program)
                    if !program.is_empty()
                        && program
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) =>
                {
                    Ok(Self::Process(program.to_ascii_lowercase()))
                }
                _ => Err(format!(
                    "unknown permission \"{text}\"; use fs.read, fs.write, ffmpeg, clipboard or process:<program>"
                )),
            },
        }
    }

    /// Plain words for the approval dialog.
    pub fn describe(&self) -> String {
        match self {
            Self::FsRead => "Read your files and folders".into(),
            Self::FsWrite => "Create, change, move and delete files".into(),
            Self::Process(program) => format!("Run the program \u{201c}{program}\u{201d}"),
            Self::Ffmpeg => "Run FFmpeg to read and convert media".into(),
            Self::Clipboard => "Put text on the clipboard".into(),
        }
    }
}

impl std::fmt::Display for Permission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FsRead => write!(f, "fs.read"),
            Self::FsWrite => write!(f, "fs.write"),
            Self::Process(program) => write!(f, "process:{program}"),
            Self::Ffmpeg => write!(f, "ffmpeg"),
            Self::Clipboard => write!(f, "clipboard"),
        }
    }
}

/// The permissions a plugin was granted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionSet(BTreeSet<Permission>);

impl PermissionSet {
    /// The manifest's list; invalid entries were rejected when it was parsed.
    pub fn from_manifest(list: &[String]) -> Self {
        Self(
            list.iter()
                .filter_map(|text| Permission::parse(text).ok())
                .collect(),
        )
    }

    pub fn contains(&self, permission: &Permission) -> bool {
        self.0.contains(permission)
    }

    /// Whether `program` (a name or a path) may be run.
    pub fn allows_program(&self, program: &str) -> bool {
        let name = std::path::Path::new(program)
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        self.0.contains(&Permission::Process(name))
    }

    pub fn iter(&self) -> impl Iterator<Item = &Permission> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Identifies this exact set, so a plugin asking for more after an
    /// update needs approval again.
    /// FNV-1a, which (unlike std's hasher) stays the same across Rust
    /// versions, since the value is saved in the settings.
    pub fn fingerprint(&self) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for permission in &self.0 {
            for byte in permission.to_string().bytes().chain([0]) {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0100_0000_01b3);
            }
        }
        hash
    }

    /// Error text for a host function used without its permission.
    ///
    /// # Errors
    /// The text, when the permission is missing.
    pub fn require(&self, permission: &Permission) -> Result<(), String> {
        if self.contains(permission) {
            Ok(())
        } else {
            Err(format!(
                "this needs the \"{permission}\" permission; add it to plugin.toml"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_permissions() {
        assert_eq!(Permission::parse("fs.read"), Ok(Permission::FsRead));
        assert_eq!(
            Permission::parse("process:Git"),
            Ok(Permission::Process("git".into()))
        );
        assert!(Permission::parse("process:").is_err());
        assert!(Permission::parse("process:../x").is_err());
        assert!(Permission::parse("network").is_err());
    }

    #[test]
    fn programs_match_by_name() {
        let set = PermissionSet::from_manifest(&["process:git".into()]);
        assert!(set.allows_program("git"));
        assert!(set.allows_program("C:/Program Files/Git/bin/git.exe"));
        assert!(!set.allows_program("cmd"));
    }

    #[test]
    fn fingerprint_changes_with_the_set() {
        let a = PermissionSet::from_manifest(&["fs.read".into()]);
        let b = PermissionSet::from_manifest(&["fs.read".into(), "fs.write".into()]);
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_eq!(
            a.fingerprint(),
            PermissionSet::from_manifest(&["fs.read".into()]).fingerprint()
        );
    }
}
