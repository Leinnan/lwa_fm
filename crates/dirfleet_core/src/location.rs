//! Where a tab or an item is: a folder on this machine, a place inside an
//! archive, or a place on a server.
//!
//! Everywhere else a location stays the `String` it always was (a tab's path,
//! an event's `path`, the saved state), so a folder on this machine is still
//! just its path. This module reads the other two forms out of such a string
//! and writes them back:
//!
//! - `sftp://<connection>/dir/file` (also `ftp`, `dav` and `s3`): a place on
//!   a saved connection, named by the connection's id, so that no host or
//!   user name sits in tabs, recent folders or logs;
//! - `archive:<outer>!/dir/file`: a place inside the archive at `<outer>`,
//!   which is itself a location with its `%` and `!` escaped, so archives
//!   nest and the first `!/` always ends the outer part.
use std::fmt;
use std::path::{MAIN_SEPARATOR, Path};

use facet::Facet;
use serde::{Deserialize, Serialize};

const ARCHIVE_PREFIX: &str = "archive:";
const ARCHIVE_SEPARATOR: &str = "!/";

/// How a server is talked to.
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[repr(C)]
pub enum Scheme {
    #[default]
    Sftp,
    Ftp,
    Dav,
    S3,
}

impl Scheme {
    pub const ALL: [Self; 4] = [Self::Sftp, Self::Ftp, Self::Dav, Self::S3];

    /// The scheme as a location's text starts with it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sftp => "sftp",
            Self::Ftp => "ftp",
            Self::Dav => "dav",
            Self::S3 => "s3",
        }
    }
}

/// The three sorts of place, for a shell that draws them differently.
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(C)]
pub enum LocationKind {
    Local,
    Archive,
    Remote,
}

/// What can be done in a place. A shell greys out what a place can't do; the
/// core refuses it.
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "what a place can do, flag by flag"
)]
pub struct Capabilities {
    /// Items can be made, changed and removed.
    pub writable: bool,
    /// Removed items go to the Trash rather than away for good.
    pub trash: bool,
    /// Items can be renamed in place.
    pub rename: bool,
    /// Thumbnails are made without being asked for.
    pub thumbnails: bool,
    /// Items are files on this machine, so other apps can be handed their
    /// paths (Open With, a terminal, a drag out of the window).
    pub real_paths: bool,
    /// Subfolders can be searched.
    pub search_walk: bool,
}

/// A place: see the module's description for how each is written as text.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Location {
    /// A folder or file on this machine, by its path.
    Local(String),
    /// A place inside the archive at `outer`. `inner` is separated by `/` and
    /// is empty at the archive's top.
    Archive { outer: Box<Self>, inner: String },
    /// A place on the saved connection `connection`. `path` starts with `/`.
    Remote {
        scheme: Scheme,
        connection: String,
        path: String,
    },
}

impl Location {
    /// Reads a location from its text. Text that is not a server or archive
    /// location is a path on this machine, so this never fails.
    pub fn parse(text: &str) -> Self {
        Self::parse_remote(text)
            .or_else(|| Self::parse_archive(text))
            .unwrap_or_else(|| Self::Local(text.to_owned()))
    }

    fn parse_remote(text: &str) -> Option<Self> {
        let (scheme, rest) = Scheme::ALL.into_iter().find_map(|scheme| {
            let rest = text.strip_prefix(scheme.as_str())?.strip_prefix("://")?;
            Some((scheme, rest))
        })?;
        let (connection, path) = rest
            .find('/')
            .map_or((rest, ""), |index| rest.split_at(index));
        if connection.is_empty() {
            return None;
        }
        Some(Self::Remote {
            scheme,
            connection: connection.to_owned(),
            path: format!("/{}", path.trim_matches('/')),
        })
    }

    fn parse_archive(text: &str) -> Option<Self> {
        let rest = text.strip_prefix(ARCHIVE_PREFIX)?;
        let (outer, inner) = rest.split_once(ARCHIVE_SEPARATOR)?;
        if outer.is_empty() {
            return None;
        }
        Some(Self::Archive {
            outer: Box::new(Self::parse(&unescape(outer))),
            inner: inner.trim_matches('/').to_owned(),
        })
    }

    /// The top of the archive that is the file at this location.
    #[must_use]
    pub fn into_archive(self) -> Self {
        Self::Archive {
            outer: Box::new(self),
            inner: String::new(),
        }
    }

    pub const fn kind(&self) -> LocationKind {
        match self {
            Self::Local(_) => LocationKind::Local,
            Self::Archive { .. } => LocationKind::Archive,
            Self::Remote { .. } => LocationKind::Remote,
        }
    }

    /// The path, when the place is on this machine.
    pub const fn as_local(&self) -> Option<&str> {
        match self {
            Self::Local(path) => Some(path.as_str()),
            _ => None,
        }
    }

    /// The place holding this one. Going up from an archive's top leaves the
    /// archive for the folder it is in; a server's root has nothing above it.
    pub fn parent(&self) -> Option<Self> {
        match self {
            Self::Local(path) => {
                let parent = Path::new(path).parent()?.to_str()?;
                (!parent.is_empty()).then(|| Self::Local(parent.to_owned()))
            }
            Self::Archive { outer, inner } if inner.is_empty() => outer.parent(),
            Self::Archive { outer, inner } => Some(Self::Archive {
                outer: outer.clone(),
                inner: inner.rsplit_once('/').map_or("", |(dir, _)| dir).to_owned(),
            }),
            Self::Remote { path, .. } if path == "/" => None,
            Self::Remote {
                scheme,
                connection,
                path,
            } => {
                let dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
                Some(Self::Remote {
                    scheme: *scheme,
                    connection: connection.clone(),
                    path: if dir.is_empty() { "/" } else { dir }.to_owned(),
                })
            }
        }
    }

    /// The item called `name` in this place. A path on this machine keeps
    /// the separator it was written with.
    #[must_use]
    pub fn join(&self, name: &str) -> Self {
        match self {
            Self::Local(path) => Self::Local(join_local(path, name)),
            Self::Archive { outer, inner } => Self::Archive {
                outer: outer.clone(),
                inner: if inner.is_empty() {
                    name.to_owned()
                } else {
                    format!("{inner}/{name}")
                },
            },
            Self::Remote {
                scheme,
                connection,
                path,
            } => Self::Remote {
                scheme: *scheme,
                connection: connection.clone(),
                path: if path == "/" {
                    format!("/{name}")
                } else {
                    format!("{path}/{name}")
                },
            },
        }
    }

    /// The place's own name: its last part, or the archive's name at an
    /// archive's top. A root has none.
    pub fn file_name(&self) -> Option<&str> {
        match self {
            Self::Local(path) => Path::new(path).file_name()?.to_str(),
            Self::Archive { outer, inner } if inner.is_empty() => outer.file_name(),
            Self::Archive { inner, .. } => Some(last_part(inner)),
            Self::Remote { path, .. } if path == "/" => None,
            Self::Remote { path, .. } => Some(last_part(path)),
        }
    }

    /// What a tab showing the place is called: its name, or for a root the
    /// path itself or the connection's id.
    pub fn title(&self) -> String {
        self.file_name().map_or_else(
            || match self {
                Self::Local(path) => path.clone(),
                Self::Archive { outer, .. } => outer.title(),
                Self::Remote { connection, .. } => connection.clone(),
            },
            str::to_owned,
        )
    }

    /// Whether this place is `other` or somewhere below it. An archive's
    /// contents are below the folder the archive is in.
    pub fn starts_with(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Local(path), Self::Local(base)) => Path::new(path).starts_with(base),
            (Self::Local(_), _) => false,
            _ => {
                self == other
                    || self
                        .parent()
                        .is_some_and(|parent| parent.starts_with(other))
            }
        }
    }

    /// The connection whose operations the place's own wait behind. A place
    /// on this machine has none: its operations line up by volume.
    pub fn lane_key(&self) -> Option<&str> {
        match self {
            Self::Local(_) => None,
            Self::Archive { outer, .. } => outer.lane_key(),
            Self::Remote { connection, .. } => Some(connection),
        }
    }

    /// What the sort of place can do, before anything is known about the
    /// place itself (a folder may still turn out to be read-only).
    pub const fn default_caps(&self) -> Capabilities {
        match self {
            Self::Local(_) => Capabilities {
                writable: true,
                trash: true,
                rename: true,
                thumbnails: true,
                real_paths: true,
                search_walk: true,
            },
            Self::Archive { .. } => Capabilities {
                writable: false,
                trash: false,
                rename: false,
                thumbnails: false,
                real_paths: false,
                search_walk: true,
            },
            Self::Remote { .. } => Capabilities {
                writable: true,
                trash: false,
                rename: true,
                thumbnails: false,
                real_paths: false,
                search_walk: false,
            },
        }
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(path) => f.write_str(path),
            Self::Archive { outer, inner } => write!(
                f,
                "{ARCHIVE_PREFIX}{}{ARCHIVE_SEPARATOR}{inner}",
                escape(&outer.to_string())
            ),
            Self::Remote {
                scheme,
                connection,
                path,
            } => write!(f, "{}://{connection}{path}", scheme.as_str()),
        }
    }
}

/// The part of a `/`-separated path after its last `/`.
fn last_part(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// `name` in the folder `dir` on this machine. Where both `/` and `\`
/// separate (Windows), a path written with `/` alone keeps it.
fn join_local(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        return name.to_owned();
    }
    if dir.ends_with(std::path::is_separator) {
        return format!("{dir}{name}");
    }
    let separator = if cfg!(windows) && dir.contains('/') && !dir.contains('\\') {
        '/'
    } else {
        MAIN_SEPARATOR
    };
    format!("{dir}{separator}{name}")
}

/// Hides the characters that end an archive's outer part.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '%' => escaped.push_str("%25"),
            '!' => escaped.push_str("%21"),
            other => escaped.push(other),
        }
    }
    escaped
}

fn unescape(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((before, after)) = rest.split_once('%') {
        plain.push_str(before);
        if let Some(tail) = after.strip_prefix("25") {
            plain.push('%');
            rest = tail;
        } else if let Some(tail) = after.strip_prefix("21") {
            plain.push('!');
            rest = tail;
        } else {
            plain.push('%');
            rest = after;
        }
    }
    plain.push_str(rest);
    plain
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(path: &str) -> Location {
        Location::Local(path.to_owned())
    }

    #[test]
    fn a_path_is_a_place_on_this_machine_and_reads_back_unchanged() {
        for path in ["/a/b", "C:\\Users\\me", "relative/dir", ""] {
            let location = Location::parse(path);
            assert_eq!(location, local(path));
            assert_eq!(location.as_local(), Some(path));
            assert_eq!(location.to_string(), path);
        }
    }

    #[test]
    fn a_server_location_names_its_connection_and_path() {
        let location = Location::parse("sftp://work/srv/www/");
        assert_eq!(
            location,
            Location::Remote {
                scheme: Scheme::Sftp,
                connection: "work".to_owned(),
                path: "/srv/www".to_owned(),
            }
        );
        assert_eq!(location.kind(), LocationKind::Remote);
        assert_eq!(location.as_local(), None);
        assert_eq!(location.to_string(), "sftp://work/srv/www");
        // A connection alone is its root.
        assert_eq!(Location::parse("s3://photos").to_string(), "s3://photos/");
        for scheme in Scheme::ALL {
            let text = format!("{}://id/dir", scheme.as_str());
            assert_eq!(Location::parse(&text).to_string(), text);
        }
    }

    #[test]
    fn an_archive_location_holds_the_archive_s_own_location_and_nests() {
        let archive = local("/a/50%!/x.zip").into_archive().join("docs");
        let text = archive.to_string();
        assert_eq!(text, "archive:/a/50%25%21/x.zip!/docs");
        assert_eq!(Location::parse(&text), archive);

        // An archive inside that one, on a server for good measure.
        let remote = Location::parse("sftp://work/backups/all.tar");
        let nested = remote
            .into_archive()
            .join("old")
            .join("x.zip")
            .into_archive()
            .join("readme.txt");
        let text = nested.to_string();
        assert_eq!(Location::parse(&text), nested);
        assert_eq!(nested.kind(), LocationKind::Archive);
        assert_eq!(nested.lane_key(), Some("work"));
    }

    #[test]
    fn text_that_only_looks_like_a_location_is_a_path() {
        for text in [
            "sftp://",
            "sftp:/work/x",
            "archive:",
            "archive:/a/x.zip",
            "archive:!/x",
        ] {
            assert_eq!(Location::parse(text), local(text), "{text}");
        }
    }

    #[test]
    fn going_up_leaves_an_archive_for_the_folder_it_is_in() {
        let inside = Location::parse("archive:/a/x.zip!/docs/img");
        let docs = inside.parent().expect("docs");
        assert_eq!(docs.to_string(), "archive:/a/x.zip!/docs");
        let top = docs.parent().expect("the archive's top");
        assert_eq!(top.to_string(), "archive:/a/x.zip!/");
        assert_eq!(top.parent(), Some(local("/a")));
    }

    #[test]
    fn going_up_stops_at_a_root() {
        let dir = Location::parse("ftp://nas/pub/music");
        let up = dir.parent().expect("pub");
        assert_eq!(up.to_string(), "ftp://nas/pub");
        let root = up.parent().expect("the root");
        assert_eq!(root.to_string(), "ftp://nas/");
        assert_eq!(root.parent(), None);
        assert_eq!(local("/").parent(), None);
        assert_eq!(local("name").parent(), None);
    }

    #[test]
    fn joining_keeps_the_separator_a_path_was_written_with() {
        assert_eq!(local("/a/b").join("c"), local("/a/b/c"));
        assert_eq!(local("/").join("c"), local("/c"));
        assert_eq!(local("").join("c"), local("c"));
        if cfg!(windows) {
            assert_eq!(local("C:\\a").join("b"), local("C:\\a\\b"));
            assert_eq!(local("C:\\").join("b"), local("C:\\b"));
            assert_eq!(local("C:").join("b"), local("C:\\b"));
        }
        assert_eq!(
            Location::parse("dav://box/")
                .join("a")
                .join("b")
                .to_string(),
            "dav://box/a/b"
        );
    }

    #[test]
    fn a_place_is_called_by_its_last_part() {
        assert_eq!(local("/a/Projects").title(), "Projects");
        assert_eq!(local("/").title(), "/");
        assert_eq!(Location::parse("archive:/a/x.zip!/docs").title(), "docs");
        // An archive's top is called what the archive is.
        assert_eq!(Location::parse("archive:/a/x.zip!/").title(), "x.zip");
        assert_eq!(Location::parse("sftp://work/srv/www").title(), "www");
        assert_eq!(Location::parse("sftp://work/").file_name(), None);
        assert_eq!(Location::parse("sftp://work/").title(), "work");
    }

    #[test]
    fn a_place_is_below_the_places_above_it() {
        let folder = local("/a");
        let inside = Location::parse("archive:/a/x.zip!/docs/img");
        let docs = Location::parse("archive:/a/x.zip!/docs");
        assert!(inside.starts_with(&docs));
        assert!(inside.starts_with(&inside));
        assert!(inside.starts_with(&folder));
        assert!(!docs.starts_with(&inside));
        assert!(!folder.starts_with(&docs));
        assert!(local("/a/b").starts_with(&folder));
        assert!(!local("/ab").starts_with(&folder));
        let server = Location::parse("sftp://work/srv");
        assert!(Location::parse("sftp://work/srv/www").starts_with(&server));
        assert!(!Location::parse("sftp://home/srv/www").starts_with(&server));
        assert!(!Location::parse("ftp://work/srv/www").starts_with(&server));
    }

    #[test]
    fn each_sort_of_place_says_what_it_can_do() {
        let here = local("/a").default_caps();
        assert!(here.writable && here.trash && here.real_paths && here.thumbnails);
        let archive = Location::parse("archive:/a/x.zip!/").default_caps();
        assert!(!archive.writable && !archive.rename && !archive.real_paths);
        assert!(archive.search_walk);
        let server = Location::parse("sftp://work/").default_caps();
        assert!(server.writable && server.rename);
        assert!(!server.trash && !server.real_paths && !server.search_walk);
        assert_eq!(local("/a").lane_key(), None);
        assert_eq!(Location::parse("sftp://work/x").lane_key(), Some("work"));
    }
}
