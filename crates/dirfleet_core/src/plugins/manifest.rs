//! `plugin.toml`: what a plugin is, what it may do and what it adds to the app.
//!
//! Parsing is strict (`deny_unknown_fields`), so a typo is an error instead
//! of a setting that silently does nothing.

use super::FieldDef;
use super::permissions::Permission;
use serde::Deserialize;

/// The plugin API versions this build runs.
pub const SUPPORTED_API_VERSIONS: &[u32] = &[1];

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub api_version: u32,
    /// Reverse-DNS identifier, e.g. `io.github.someone.git`.
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Folders where the directory bar and folder actions apply.
    #[serde(default)]
    pub activation: Activation,
    #[serde(default, rename = "action")]
    pub actions: Vec<ActionDef>,
    pub bar: Option<BarDef>,
    pub inspector: Option<InspectorDef>,
    #[serde(default)]
    pub events: EventsDef,
    /// How many jobs of this plugin may run at once.
    #[serde(default = "one")]
    pub max_jobs: usize,
}

const fn one() -> usize {
    1
}

const fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Activation {
    #[serde(default)]
    pub always: bool,
    /// The folder contains one of these names.
    #[serde(default)]
    pub contains_any: Vec<String>,
    /// The folder or one of its parents contains one of these names.
    #[serde(default)]
    pub ancestor_contains: Vec<String>,
    /// The folder contains an item matching this glob.
    pub glob: Option<String>,
}

impl Activation {
    /// No conditions: the plugin applies everywhere, without looking.
    #[must_use]
    pub const fn everywhere(&self) -> bool {
        self.always
            || (self.contains_any.is_empty()
                && self.ancestor_contains.is_empty()
                && self.glob.is_none())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MenuPlace {
    #[default]
    None,
    /// The menu of selected items.
    Entry,
    /// The menu of the folder's empty area.
    Background,
    Both,
}

impl MenuPlace {
    pub const fn in_entry_menu(self) -> bool {
        matches!(self, Self::Entry | Self::Both)
    }
    pub const fn in_background_menu(self) -> bool {
        matches!(self, Self::Background | Self::Both)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    /// Selected files and folders.
    #[default]
    Any,
    Files,
    Dirs,
    /// The folder being shown; the selection doesn't matter.
    CurrentDir,
}

/// Which items an action applies to.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Targets {
    #[serde(default)]
    pub kind: TargetKind,
    /// File extensions, without the dot; any case.
    #[serde(default)]
    pub ext: Vec<String>,
    pub min: Option<usize>,
    pub max: Option<usize>,
    /// A glob every target name must match.
    pub glob: Option<String>,
}

/// One item an action runs on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetInfo {
    pub path: std::path::PathBuf,
    pub is_dir: bool,
    pub size: u64,
    /// Seconds since the Unix epoch, 0 when unknown.
    pub modified: i64,
}

impl TargetInfo {
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
    pub fn ext(&self) -> String {
        if self.is_dir {
            return String::new();
        }
        self.path
            .extension()
            .map(|ext| ext.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

impl Targets {
    /// Whether the action applies to these selected items (in a folder,
    /// when one is shown).
    pub fn matches(&self, items: &[TargetInfo], has_dir: bool) -> bool {
        if self.kind == TargetKind::CurrentDir {
            return has_dir;
        }
        let min = self.min.unwrap_or(1);
        if items.len() < min || self.max.is_some_and(|max| items.len() > max) {
            return false;
        }
        let pattern = self
            .glob
            .as_ref()
            .and_then(|glob| glob::Pattern::new(glob).ok());
        items.iter().all(|item| {
            let kind = match self.kind {
                TargetKind::Files => !item.is_dir,
                TargetKind::Dirs => item.is_dir,
                TargetKind::Any | TargetKind::CurrentDir => true,
            };
            let ext = self.ext.is_empty()
                || (!item.is_dir
                    && self
                        .ext
                        .iter()
                        .any(|ext| ext.eq_ignore_ascii_case(&item.ext())));
            let glob = pattern
                .as_ref()
                .is_none_or(|pattern| pattern.matches(&item.name()));
            kind && ext && glob
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FormMode {
    #[default]
    None,
    /// `M.actions.<id>.form(ctx)` returns the fields.
    Dynamic,
    /// The fields are listed in the manifest as `[[action.field]]`.
    Static,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    /// Runs on the plugin's worker with a short time budget.
    #[default]
    Quick,
    /// Runs on a job thread with progress and cancel.
    Job,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionDef {
    pub id: String,
    pub title: String,
    pub icon: Option<String>,
    /// Default shortcut such as `Ctrl+Alt+M`; users can change it.
    pub shortcut: Option<String>,
    #[serde(default = "yes")]
    pub palette: bool,
    #[serde(default)]
    pub context_menu: MenuPlace,
    #[serde(default)]
    pub targets: Targets,
    #[serde(default)]
    pub form: FormMode,
    #[serde(default, rename = "field")]
    pub fields: Vec<FieldDef>,
    #[serde(default)]
    pub mode: RunMode,
    /// Run `run_item` once per target instead of `run` once.
    #[serde(default)]
    pub per_item: bool,
    /// Ask before running, with this text.
    pub confirm: Option<String>,
    /// Open a dialog holding the options and the results `show_table` sends,
    /// which can stop, restart or leave the job running in the background.
    #[serde(default)]
    pub results: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BarDef {
    #[serde(default = "bar_handler")]
    pub handler: String,
    /// Refresh the bar this often while it's shown.
    pub refresh_secs: Option<u64>,
}

fn bar_handler() -> String {
    "bar".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InspectorDef {
    pub title: String,
    #[serde(default = "inspector_handler")]
    pub handler: String,
    #[serde(default)]
    pub targets: Targets,
}

fn inspector_handler() -> String {
    "inspector".into()
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsDef {
    #[serde(default)]
    pub subscribe: Vec<String>,
}

/// Events a plugin can subscribe to; each calls `M.on_<name>(ctx, data)`.
pub const EVENTS: &[&str] = &[
    "app_started",
    "folder_opened",
    "selection_changed",
    "fs_changed",
    "file_op_finished",
    "job_finished",
];

/// A problem in a plugin's files, with where it is when known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub file: String,
    pub line: Option<usize>,
    pub message: String,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(line) => write!(f, "{}:{line}: {}", self.file, self.message),
            None => write!(f, "{}: {}", self.file, self.message),
        }
    }
}

pub const MANIFEST_FILE: &str = "plugin.toml";
pub const MAIN_FILE: &str = "main.lua";

/// Parses and checks a manifest.
///
/// # Errors
/// What is wrong with it, and where.
pub fn parse(text: &str) -> Result<Manifest, Diagnostic> {
    let manifest: Manifest = toml::from_str(text).map_err(|error| Diagnostic {
        file: MANIFEST_FILE.into(),
        line: error
            .span()
            .map(|span| text[..span.start.min(text.len())].matches('\n').count() + 1),
        message: error.message().trim().to_owned(),
    })?;
    validate(&manifest).map_err(|message| Diagnostic {
        file: MANIFEST_FILE.into(),
        line: None,
        message,
    })?;
    Ok(manifest)
}

#[allow(clippy::too_many_lines)]
fn validate(manifest: &Manifest) -> Result<(), String> {
    if !SUPPORTED_API_VERSIONS.contains(&manifest.api_version) {
        return Err(format!(
            "api_version {} is not supported; this version of DirFleet runs api_version {}",
            manifest.api_version,
            SUPPORTED_API_VERSIONS
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !is_valid_id(&manifest.id) {
        return Err(format!(
            "id \"{}\" must be reverse-DNS: lowercase letters, digits and '-', at least two parts separated by '.'",
            manifest.id
        ));
    }
    if manifest.name.trim().is_empty() || manifest.name.chars().count() > 64 {
        return Err("name must be 1 to 64 characters".into());
    }
    if manifest.author.trim().is_empty() {
        return Err("author must not be empty".into());
    }
    if !is_semver(&manifest.version) {
        return Err(format!(
            "version \"{}\" must be MAJOR.MINOR.PATCH",
            manifest.version
        ));
    }
    if manifest.max_jobs == 0 {
        return Err("max_jobs must be at least 1".into());
    }
    for permission in &manifest.permissions {
        Permission::parse(permission)?;
    }
    for (index, action) in manifest.actions.iter().enumerate() {
        if !is_identifier(&action.id) {
            return Err(format!(
                "action id \"{}\" must be lowercase letters, digits, '_' or '-'",
                action.id
            ));
        }
        if manifest.actions[..index]
            .iter()
            .any(|other| other.id == action.id)
        {
            return Err(format!("action id \"{}\" is used twice", action.id));
        }
        if action.title.trim().is_empty() {
            return Err(format!("action \"{}\" needs a title", action.id));
        }
        if let Some(shortcut) = &action.shortcut {
            super::shortcut::parse(shortcut)
                .map_err(|error| format!("action \"{}\": {error}", action.id))?;
        }
        if let (Some(min), Some(max)) = (action.targets.min, action.targets.max)
            && min > max
        {
            return Err(format!(
                "action \"{}\": targets.min > targets.max",
                action.id
            ));
        }
        match action.form {
            FormMode::Static if action.fields.is_empty() => {
                return Err(format!(
                    "action \"{}\" has form = \"static\" but no [[action.field]]",
                    action.id
                ));
            }
            FormMode::None | FormMode::Dynamic if !action.fields.is_empty() => {
                return Err(format!(
                    "action \"{}\" lists fields, so it needs form = \"static\"",
                    action.id
                ));
            }
            _ => {}
        }
        if action.form == FormMode::Static {
            super::Form {
                fields: action.fields.clone(),
                ..Default::default()
            }
            .check()
            .map_err(|error| format!("action \"{}\": {error}", action.id))?;
        }
        if action.results
            && (action.mode != RunMode::Job
                || action.per_item
                || action.form == FormMode::Dynamic
                || action.confirm.is_some())
        {
            return Err(format!(
                "action \"{}\": results needs mode = \"job\", no per_item or confirm, and a static form or none",
                action.id
            ));
        }
        if action.per_item && action.mode != RunMode::Job {
            return Err(format!(
                "action \"{}\": per_item needs mode = \"job\"",
                action.id
            ));
        }
        if let Some(pattern) = &action.targets.glob {
            glob::Pattern::new(pattern)
                .map_err(|error| format!("action \"{}\": bad glob: {error}", action.id))?;
        }
    }
    if let Some(pattern) = &manifest.activation.glob {
        glob::Pattern::new(pattern).map_err(|error| format!("activation.glob: {error}"))?;
    }
    for event in &manifest.events.subscribe {
        if !EVENTS.contains(&event.as_str()) {
            return Err(format!(
                "unknown event \"{event}\"; known events: {}",
                EVENTS.join(", ")
            ));
        }
    }
    Ok(())
}

fn is_identifier(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

pub fn is_valid_id(id: &str) -> bool {
    let parts: Vec<&str> = id.split('.').collect();
    id.len() <= 128
        && parts.len() >= 2
        && parts.iter().all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

fn is_semver(version: &str) -> bool {
    let core = version
        .split_once(['-', '+'])
        .map_or(version, |(core, _)| core);
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
api_version = 1
id = "io.example.test"
name = "Test"
version = "1.0.0"
author = "Me"
"#;

    #[test]
    fn minimal_manifest_parses() {
        let manifest = parse(MINIMAL).expect("valid");
        assert_eq!(manifest.id, "io.example.test");
        assert_eq!(manifest.max_jobs, 1);
        assert!(manifest.actions.is_empty());
    }

    #[test]
    fn unknown_keys_are_errors_with_a_line() {
        let error = parse(&format!("{MINIMAL}colour = \"red\"\n")).expect_err("unknown key");
        assert!(error.message.contains("colour"), "{error}");
        assert_eq!(error.line, Some(7));
    }

    #[test]
    fn rejects_bad_ids_versions_and_api() {
        assert!(parse(&MINIMAL.replace("io.example.test", "Test")).is_err());
        assert!(parse(&MINIMAL.replace("1.0.0", "1.0")).is_err());
        let error = parse(&MINIMAL.replace("api_version = 1", "api_version = 9")).expect_err("api");
        assert!(error.message.contains("api_version 9"));
    }

    #[test]
    fn checks_actions() {
        let duplicate = format!(
            "{MINIMAL}[[action]]\nid = \"a\"\ntitle = \"A\"\n[[action]]\nid = \"a\"\ntitle = \"B\"\n"
        );
        assert!(
            parse(&duplicate)
                .expect_err("dup")
                .message
                .contains("twice")
        );
        for bad in [
            "",
            "mode = \"job\"\nform = \"dynamic\"\n",
            "mode = \"job\"\nper_item = true\n",
        ] {
            let results =
                format!("{MINIMAL}[[action]]\nid = \"a\"\ntitle = \"A\"\nresults = true\n{bad}");
            assert!(parse(&results).is_err(), "{bad}");
        }
        let results = format!(
            "{MINIMAL}[[action]]\nid = \"a\"\ntitle = \"A\"\nresults = true\nmode = \"job\"\n"
        );
        assert!(parse(&results).is_ok());
        let per_item = format!("{MINIMAL}[[action]]\nid = \"a\"\ntitle = \"A\"\nper_item = true\n");
        assert!(parse(&per_item).is_err());
        let shortcut =
            format!("{MINIMAL}[[action]]\nid = \"a\"\ntitle = \"A\"\nshortcut = \"Ctrl+Nope\"\n");
        assert!(parse(&shortcut).is_err());
        let permission = format!("{MINIMAL}permissions = [\"root\"]\n");
        assert!(parse(&permission).is_err());
    }

    fn file(path: &str, is_dir: bool) -> TargetInfo {
        TargetInfo {
            path: path.into(),
            is_dir,
            size: 0,
            modified: 0,
        }
    }

    #[test]
    fn targets_match_kind_ext_and_count() {
        let videos = Targets {
            kind: TargetKind::Files,
            ext: vec!["mov".into(), "avi".into()],
            ..Default::default()
        };
        assert!(videos.matches(&[file("a/clip.MOV", false)], true));
        assert!(videos.matches(&[file("a/x.mov", false), file("a/y.avi", false)], true));
        assert!(!videos.matches(&[file("a/x.mov", false), file("a/y.txt", false)], true));
        assert!(!videos.matches(&[file("a/x.mov", true)], true));
        assert!(!videos.matches(&[], true));
        let one = Targets {
            max: Some(1),
            ..Default::default()
        };
        assert!(!one.matches(&[file("a", true), file("b", false)], true));
        let dir = Targets {
            kind: TargetKind::CurrentDir,
            ..Default::default()
        };
        assert!(dir.matches(&[], true));
        assert!(!dir.matches(&[], false));
    }
}
