//! The text form of a search: `report ext:pdf,docx size:>10mb -draft`.
//!
//! Every chip has a token, so a query can be typed or pasted and the chips
//! can always be shown as text (tooltips, saved search names, the palette).

use super::{Age, Chip, Cmp, FileKind, MatchMode, Search, SearchTermType};
use std::path::PathBuf;

/// The result of parsing typed text.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub chips: Vec<Chip>,
    /// The words were joined with `OR`.
    pub any: bool,
}

/// The keywords understood by [`parse`], for the syntax help.
pub const HELP: &[(&str, &str)] = &[
    ("report", "Name contains \"report\""),
    ("\"annual report\"", "Name contains the exact phrase"),
    ("*.pdf", "Name matches a wildcard pattern"),
    ("/^IMG_\\d+/", "Name matches a regular expression"),
    ("-draft", "Name does not contain \"draft\""),
    ("a OR b", "Either word (otherwise all words must match)"),
    ("ext:pdf,docx", "File extension"),
    (
        "kind:image,video",
        "folder, image, video, audio, doc, archive",
    ),
    ("size:>10mb", "Size; also <, <=, >=, = and kb, mb, gb"),
    ("modified:week", "today, week, month, year"),
    ("in:\"D:\\Work\"", "Also search this folder"),
];

/// Splits on whitespace, keeping "quoted text" together.
fn tokens(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in input.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                current.push(c);
            }
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn unquote(text: &str) -> &str {
    text.strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(text)
}

fn parse_size(text: &str) -> Option<(Cmp, u64)> {
    let (cmp, rest) = [
        (">=", Cmp::GreaterOrEqual),
        ("<=", Cmp::LessOrEqual),
        (">", Cmp::Greater),
        ("<", Cmp::Less),
        ("=", Cmp::Equal),
    ]
    .into_iter()
    .find_map(|(prefix, cmp)| text.strip_prefix(prefix).map(|rest| (cmp, rest)))
    .unwrap_or((Cmp::GreaterOrEqual, text));
    let rest = rest.to_ascii_lowercase();
    let digits = rest
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(rest.len());
    let (number, unit) = rest.split_at(digits);
    let number: f64 = number.parse().ok()?;
    let multiplier: f64 = match unit.trim() {
        "" | "b" => 1.0,
        "k" | "kb" => 1024.0,
        "m" | "mb" => 1024.0 * 1024.0,
        "g" | "gb" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((cmp, (number * multiplier) as u64))
}

/// "10mb" style text for a byte count.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [(&str, u64); 3] = [("gb", 1 << 30), ("mb", 1 << 20), ("kb", 1 << 10)];
    for (unit, size) in UNITS {
        if bytes >= size && bytes.is_multiple_of(size) {
            return format!("{}{unit}", bytes / size);
        }
    }
    for (unit, size) in UNITS {
        if bytes >= size {
            return format!("{:.1}{unit}", bytes as f64 / size as f64);
        }
    }
    format!("{bytes}b")
}

/// Turns one `key:value` token into a chip, if it is one.
pub fn parse_filter(token: &str) -> Option<Chip> {
    let (key, value) = token.split_once(':')?;
    let value = unquote(value);
    if value.is_empty() {
        return None;
    }
    match key.to_ascii_lowercase().as_str() {
        "ext" => Some(Chip::Ext(
            value
                .split(',')
                .map(|ext| ext.trim().trim_start_matches('.').to_lowercase())
                .filter(|ext| !ext.is_empty())
                .collect(),
        )),
        "kind" | "type" => {
            let kinds: Option<Vec<FileKind>> = value
                .split(',')
                .filter(|word| !word.trim().is_empty())
                .map(|word| {
                    FileKind::ALL.into_iter().find(|kind| {
                        kind.keyword().eq_ignore_ascii_case(word.trim())
                            || kind.label().eq_ignore_ascii_case(word.trim())
                    })
                })
                .collect();
            kinds.filter(|kinds| !kinds.is_empty()).map(Chip::Kind)
        }
        "size" => parse_size(value).map(|(cmp, bytes)| Chip::Size { cmp, bytes }),
        "modified" | "date" => Age::ALL
            .into_iter()
            .find(|age| age.keyword().eq_ignore_ascii_case(value))
            .map(Chip::Modified),
        "in" => Some(Chip::In(PathBuf::from(value))),
        _ => None,
    }
}

/// Parses typed text into chips. Unknown `key:value` tokens stay name text.
pub fn parse(input: &str) -> Parsed {
    let mut parsed = Parsed::default();
    for token in tokens(input) {
        if token == "OR" {
            parsed.any = true;
            continue;
        }
        if let Some(chip) = parse_filter(&token) {
            parsed.chips.push(chip);
            continue;
        }
        let (exclude, text) = match token.strip_prefix('-') {
            Some(rest) if !rest.is_empty() => (true, rest),
            _ => (false, token.as_str()),
        };
        let (pattern, kind) = if text.len() > 2 && text.starts_with('/') && text.ends_with('/') {
            (&text[1..text.len() - 1], SearchTermType::Regex)
        } else if text.starts_with('"') {
            (unquote(text), SearchTermType::Plain)
        } else if text.contains(['*', '?']) {
            (text, SearchTermType::Glob)
        } else {
            (text, SearchTermType::Plain)
        };
        parsed.chips.push(Chip::Name {
            pattern: pattern.to_owned(),
            kind,
            exclude,
        });
    }
    parsed
}

fn quote_if_needed(text: &str) -> String {
    if text.contains(char::is_whitespace) {
        format!("\"{text}\"")
    } else {
        text.to_owned()
    }
}

/// The token that [`parse`] turns back into `chip`.
pub fn chip_token(chip: &Chip) -> String {
    match chip {
        Chip::Name {
            pattern,
            kind,
            exclude,
        } => {
            let body = match kind {
                SearchTermType::Regex => format!("/{pattern}/"),
                SearchTermType::Glob => pattern.clone(),
                SearchTermType::Plain if pattern.contains(['*', '?']) || pattern.contains(':') => {
                    format!("\"{pattern}\"")
                }
                SearchTermType::Plain => quote_if_needed(pattern),
            };
            if *exclude { format!("-{body}") } else { body }
        }
        Chip::Ext(exts) => format!("ext:{}", exts.join(",")),
        Chip::Kind(kinds) => format!(
            "kind:{}",
            kinds
                .iter()
                .map(|kind| kind.keyword())
                .collect::<Vec<_>>()
                .join(",")
        ),
        Chip::Size { cmp, bytes } => format!("size:{}{}", cmp.symbol(), format_size(*bytes)),
        Chip::Modified(age) => format!("modified:{}", age.keyword()),
        Chip::In(path) => format!("in:{}", quote_if_needed(&path.display().to_string())),
    }
}

/// "10 MB" style text for a byte count, for people rather than the parser.
pub fn display_size(bytes: u64) -> String {
    let text = format_size(bytes);
    let unit_start = text
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(unit_start);
    match unit {
        "b" => format!("{number} bytes"),
        unit => format!("{number} {}", unit.to_ascii_uppercase()),
    }
}

/// What a size comparison reads as on a chip: "over", "at most", …
pub const fn cmp_words(cmp: Cmp) -> &'static str {
    match cmp {
        Cmp::Less => "under",
        Cmp::LessOrEqual => "at most",
        Cmp::Equal => "exactly",
        Cmp::GreaterOrEqual => "at least",
        Cmp::Greater => "over",
    }
}

/// A chip as people read it: what it filters ("Type", or `None` for a plain
/// word) and the value ("Image, Video").
pub fn chip_parts(chip: &Chip) -> (Option<&'static str>, String) {
    match chip {
        Chip::Name {
            pattern,
            kind,
            exclude,
        } => {
            let shown = match kind {
                SearchTermType::Regex => format!("/{pattern}/"),
                _ => pattern.clone(),
            };
            (exclude.then_some("Not"), shown)
        }
        Chip::Ext(exts) => (
            Some("Extension"),
            exts.iter()
                .map(|ext| format!(".{ext}"))
                .collect::<Vec<_>>()
                .join(", "),
        ),
        Chip::Kind(kinds) => (
            Some("Type"),
            kinds
                .iter()
                .map(|kind| kind.label())
                .collect::<Vec<_>>()
                .join(", "),
        ),
        Chip::Size { cmp, bytes } => (
            Some("Size"),
            format!("{} {}", cmp_words(*cmp), display_size(*bytes)),
        ),
        Chip::Modified(age) => (Some("Modified"), age.label().to_lowercase()),
        Chip::In(path) => (
            Some("Folder"),
            path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            ),
        ),
    }
}

/// A chip as one line of text: "Type: Image, Video", or just the word.
pub fn chip_label(chip: &Chip) -> String {
    match chip_parts(chip) {
        (Some(key), value) => format!("{key}: {value}"),
        (None, value) => value,
    }
}

/// The whole search as text, e.g. for a saved search's tooltip.
pub fn format(search: &Search) -> String {
    let names: Vec<String> = search
        .chips
        .iter()
        .filter(|chip| chip.is_name())
        .map(chip_token)
        .chain((!search.value.trim().is_empty()).then(|| search.value.trim().to_owned()))
        .collect();
    let separator = if search.match_mode == MatchMode::Any {
        " OR "
    } else {
        " "
    };
    let mut parts = vec![names.join(separator)];
    parts.extend(
        search
            .chips
            .iter()
            .filter(|chip| !chip.is_name())
            .map(chip_token),
    );
    parts.retain(|part| !part.is_empty());
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::{Chip, Cmp, chip_label, chip_token, format, parse};
    use crate::app::search::{Age, FileKind, MatchMode, Search, SearchTermType};
    use std::path::PathBuf;

    #[test]
    fn parses_filters_and_names() {
        let parsed = parse(
            r#"report "annual plan" -draft *.pdf /^IMG/ ext:.PDF,docx kind:image size:>10mb modified:week in:"D:\My Work" note:x"#,
        );
        assert!(!parsed.any);
        assert_eq!(
            parsed.chips,
            vec![
                Chip::name("report"),
                Chip::name("annual plan"),
                Chip::Name {
                    pattern: "draft".into(),
                    kind: SearchTermType::Plain,
                    exclude: true
                },
                Chip::Name {
                    pattern: "*.pdf".into(),
                    kind: SearchTermType::Glob,
                    exclude: false
                },
                Chip::Name {
                    pattern: "^IMG".into(),
                    kind: SearchTermType::Regex,
                    exclude: false
                },
                Chip::Ext(vec!["pdf".into(), "docx".into()]),
                Chip::Kind(vec![FileKind::Image]),
                Chip::Size {
                    cmp: Cmp::Greater,
                    bytes: 10 * 1024 * 1024
                },
                Chip::Modified(Age::Week),
                Chip::In(PathBuf::from(r"D:\My Work")),
                Chip::name("note:x"),
            ]
        );
        assert!(parse("a OR b").any);
    }

    #[test]
    fn every_chip_round_trips_through_its_token() {
        let chips = parse(r#"report "annual plan" -draft *.pdf /^IMG/ ext:pdf kind:doc,video size:<=1.5mb modified:today in:"C:\a b""#).chips;
        for chip in &chips {
            assert_eq!(&parse(&chip_token(chip)).chips[0], chip, "{chip:?}");
        }
    }

    #[test]
    fn formats_a_whole_search() {
        let search = Search {
            value: "c".into(),
            chips: vec![
                Chip::name("a"),
                Chip::Kind(vec![FileKind::Video]),
                Chip::name("b"),
            ],
            match_mode: MatchMode::Any,
            ..Default::default()
        };
        assert_eq!(format(&search), "a OR b OR c kind:video");
    }

    #[test]
    fn chips_read_as_words() {
        let labels: Vec<String> =
            parse("notes -draft ext:pdf,docx kind:image,video size:>10mb modified:week in:D:/Work")
                .chips
                .iter()
                .map(chip_label)
                .collect();
        assert_eq!(
            labels,
            [
                "notes",
                "Not: draft",
                "Extension: .pdf, .docx",
                "Type: Image, Video",
                "Size: over 10 MB",
                "Modified: this week",
                "Folder: Work",
            ]
        );
    }
}
