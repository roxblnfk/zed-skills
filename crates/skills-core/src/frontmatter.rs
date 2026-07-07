//! Best-effort SKILL.md frontmatter reader.
//!
//! Rules (spec §1): read the first 4096 bytes, strip a UTF-8 BOM, the
//! frontmatter must start at byte 0 (`---` line), only flat `key: value`
//! lines are consumed, one layer of surrounding quotes is stripped.
//! Anything unparseable yields an empty result — a skill is never rejected
//! because of its frontmatter.

use std::io::Read;
use std::path::Path;

/// Maximum number of bytes inspected when looking for frontmatter.
pub const READ_CAP: usize = 4096;

const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Frontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
}

/// Read frontmatter from a SKILL.md file. IO errors degrade to an empty
/// frontmatter (best-effort by contract).
pub fn read_frontmatter(skill_md: &Path) -> Frontmatter {
    let Ok(file) = std::fs::File::open(skill_md) else {
        return Frontmatter::default();
    };
    let mut buf = Vec::with_capacity(READ_CAP);
    let mut handle = file.take(READ_CAP as u64);
    if handle.read_to_end(&mut buf).is_err() {
        return Frontmatter::default();
    }
    parse_frontmatter(&buf)
}

/// Parse frontmatter from raw bytes (only the first [`READ_CAP`] bytes are
/// considered). Later occurrences of a key overwrite earlier ones (the last
/// non-absent value wins).
pub fn parse_frontmatter(bytes: &[u8]) -> Frontmatter {
    let mut fm = Frontmatter::default();
    for entry in flat_entries(bytes) {
        if entry.is_absent() {
            continue;
        }
        match entry.key.as_str() {
            "name" => fm.name = Some(entry.value),
            "description" => fm.description = Some(entry.value),
            _ => {}
        }
    }
    fm
}

/// One flat `key: value` frontmatter line as the best-effort reader sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatEntry {
    /// The key, right-trimmed (ASCII alphanumerics, `_`, `-` only).
    pub key: String,
    /// The value, trimmed with one layer of surrounding quotes stripped.
    /// May be empty — check [`FlatEntry::is_absent`] for the reader's view.
    pub value: String,
    /// 0-based line index in the document (line 0 is the opening `---`).
    pub line: usize,
}

impl FlatEntry {
    /// Whether the sync-side reader treats this value as absent: empty, or a
    /// YAML block-scalar indicator (multiline values are unsupported).
    pub fn is_absent(&self) -> bool {
        self.value.is_empty()
            || matches!(self.value.as_str(), "|" | "|-" | "|+" | ">" | ">-" | ">+")
    }
}

/// All flat `key: value` lines of the frontmatter block, in document order,
/// with 0-based line numbers. This is the single definition of the reader's
/// line rules ([`parse_frontmatter`] is built on it): first [`READ_CAP`]
/// bytes, BOM stripped, opening `---` at byte 0, closing `---` required
/// inside the window (otherwise: no frontmatter, empty result), indented
/// lines / missing colons / malformed keys skipped. Unlike
/// [`parse_frontmatter`], absent values (empty, block-scalar indicators) are
/// *included* — callers doing validation need to see them.
pub fn flat_entries(bytes: &[u8]) -> Vec<FlatEntry> {
    let mut window = &bytes[..bytes.len().min(READ_CAP)];
    if window.starts_with(BOM) {
        window = &window[BOM.len()..];
    }
    let text = String::from_utf8_lossy(window);
    let mut lines = text.lines().enumerate();

    // Frontmatter must start at byte 0.
    match lines.next() {
        Some((_, first)) if first.trim_end_matches('\r') == "---" => {}
        _ => return Vec::new(),
    }

    let mut entries = Vec::new();
    let mut closed = false;
    for (line_no, line) in lines {
        let line = line.trim_end_matches('\r');
        if line == "---" {
            closed = true;
            break;
        }
        // Flat entries only: indented (nested) lines are ignored.
        if line.starts_with([' ', '\t']) {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim_end();
        if key.is_empty()
            || !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            continue;
        }
        entries.push(FlatEntry {
            key: key.to_string(),
            value: strip_quotes(value.trim()).to_string(),
            line: line_no,
        });
    }

    // No closing delimiter inside the window: not frontmatter.
    if !closed {
        return Vec::new();
    }
    entries
}

/// Strip exactly one layer of matching surrounding quotes.
fn strip_quotes(s: &str) -> &str {
    let b = s.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Frontmatter {
        parse_frontmatter(s.as_bytes())
    }

    #[test]
    fn plain_frontmatter() {
        let fm = parse("---\nname: code-review\ndescription: Reviews code\n---\n# Body\n");
        assert_eq!(fm.name.as_deref(), Some("code-review"));
        assert_eq!(fm.description.as_deref(), Some("Reviews code"));
    }

    #[test]
    fn bom_is_stripped() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"---\nname: x\n---\n");
        let fm = parse_frontmatter(&bytes);
        assert_eq!(fm.name.as_deref(), Some("x"));
    }

    #[test]
    fn crlf_line_endings() {
        let fm = parse("---\r\nname: x\r\ndescription: y\r\n---\r\nbody");
        assert_eq!(fm.name.as_deref(), Some("x"));
        assert_eq!(fm.description.as_deref(), Some("y"));
    }

    #[test]
    fn no_frontmatter() {
        assert_eq!(
            parse("# Just a title\nname: nope\n"),
            Frontmatter::default()
        );
    }

    #[test]
    fn frontmatter_must_start_at_byte_zero() {
        assert_eq!(parse("\n---\nname: x\n---\n"), Frontmatter::default());
        assert_eq!(parse(" ---\nname: x\n---\n"), Frontmatter::default());
    }

    #[test]
    fn unclosed_frontmatter_is_ignored() {
        assert_eq!(parse("---\nname: x\n"), Frontmatter::default());
    }

    #[test]
    fn closing_delimiter_beyond_cap_is_ignored() {
        let mut doc = String::from("---\nname: x\n");
        doc.push_str(&"filler: y\n".repeat(600)); // pushes past 4096 bytes
        doc.push_str("---\n");
        assert!(doc.len() > READ_CAP);
        assert_eq!(parse(&doc), Frontmatter::default());
    }

    #[test]
    fn closing_delimiter_within_cap_works_on_large_file() {
        let mut doc = String::from("---\nname: x\n---\n");
        doc.push_str(&"body ".repeat(2000));
        assert!(doc.len() > READ_CAP);
        assert_eq!(parse(&doc).name.as_deref(), Some("x"));
    }

    #[test]
    fn double_quotes_stripped_once() {
        let fm = parse("---\nname: \"quoted\"\ndescription: \"\"nested\"\"\n---\n");
        assert_eq!(fm.name.as_deref(), Some("quoted"));
        assert_eq!(fm.description.as_deref(), Some("\"nested\""));
    }

    #[test]
    fn single_quotes_stripped() {
        let fm = parse("---\nname: 'quoted'\n---\n");
        assert_eq!(fm.name.as_deref(), Some("quoted"));
    }

    #[test]
    fn mismatched_quotes_kept() {
        let fm = parse("---\nname: \"half\n---\n");
        assert_eq!(fm.name.as_deref(), Some("\"half"));
    }

    #[test]
    fn nested_and_list_lines_ignored() {
        let fm = parse("---\nmeta:\n  name: nested\n- item\nname: real\n---\n");
        assert_eq!(fm.name.as_deref(), Some("real"));
    }

    #[test]
    fn value_with_colon_kept_whole() {
        let fm = parse("---\ndescription: use x: y wisely\n---\n");
        assert_eq!(fm.description.as_deref(), Some("use x: y wisely"));
    }

    #[test]
    fn block_scalar_indicator_treated_as_absent() {
        let fm = parse("---\ndescription: |\n  multi\n  line\nname: x\n---\n");
        assert_eq!(fm.description, None);
        assert_eq!(fm.name.as_deref(), Some("x"));
    }

    #[test]
    fn empty_value_is_none() {
        let fm = parse("---\nname:\ndescription:   \n---\n");
        assert_eq!(fm, Frontmatter::default());
    }

    #[test]
    fn unknown_keys_ignored() {
        let fm = parse("---\nversion: 2\nname: x\n---\n");
        assert_eq!(fm.name.as_deref(), Some("x"));
        assert_eq!(fm.description, None);
    }

    #[test]
    fn keys_with_spaces_ignored() {
        let fm = parse("---\nsome key: v\nname: x\n---\n");
        assert_eq!(fm.name.as_deref(), Some("x"));
    }

    #[test]
    fn empty_input() {
        assert_eq!(parse(""), Frontmatter::default());
        assert_eq!(parse("---"), Frontmatter::default());
    }

    #[test]
    fn duplicate_key_last_value_wins() {
        // Pins the reader's overwrite semantics (LSP duplicate-key
        // diagnostics reference this behavior in their message).
        let fm = parse("---\nname: first\nname: second\n---\n");
        assert_eq!(fm.name.as_deref(), Some("second"));
        // …but an absent (empty) later value does not override.
        let fm = parse("---\nname: first\nname:\n---\n");
        assert_eq!(fm.name.as_deref(), Some("first"));
    }

    #[test]
    fn flat_entries_report_lines_and_absent_values() {
        let entries = flat_entries(b"---\nname: x\n\ndescription:\nmeta:\n  nested: v\n---\n");
        assert_eq!(
            entries,
            vec![
                FlatEntry {
                    key: "name".into(),
                    value: "x".into(),
                    line: 1,
                },
                FlatEntry {
                    key: "description".into(),
                    value: String::new(),
                    line: 3,
                },
                FlatEntry {
                    key: "meta".into(),
                    value: String::new(),
                    line: 4,
                },
            ]
        );
        assert!(!entries[0].is_absent());
        assert!(entries[1].is_absent());
    }

    #[test]
    fn flat_entries_block_scalar_is_absent() {
        let entries = flat_entries(b"---\ndescription: |\n  multi\n---\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].value, "|");
        assert!(entries[0].is_absent());
    }

    #[test]
    fn flat_entries_require_block() {
        assert!(flat_entries(b"# no frontmatter\nname: x\n").is_empty());
        assert!(flat_entries(b"---\nname: x\n").is_empty()); // unclosed
    }

    #[test]
    fn missing_file_degrades_to_default() {
        let fm = read_frontmatter(Path::new("Z:/definitely/not/here/SKILL.md"));
        assert_eq!(fm, Frontmatter::default());
    }
}
