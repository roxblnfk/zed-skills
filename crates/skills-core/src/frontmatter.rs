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
/// considered).
pub fn parse_frontmatter(bytes: &[u8]) -> Frontmatter {
    let mut window = &bytes[..bytes.len().min(READ_CAP)];
    if window.starts_with(BOM) {
        window = &window[BOM.len()..];
    }
    let text = String::from_utf8_lossy(window);
    let mut lines = text.lines();

    // Frontmatter must start at byte 0.
    match lines.next() {
        Some(first) if first.trim_end_matches('\r') == "---" => {}
        _ => return Frontmatter::default(),
    }

    let mut fm = Frontmatter::default();
    let mut closed = false;
    for line in lines {
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
        let value = strip_quotes(value.trim());
        // YAML block-scalar indicators would start a multiline value, which
        // this reader does not support — treat as absent.
        if value.is_empty() || matches!(value, "|" | "|-" | "|+" | ">" | ">-" | ">+") {
            continue;
        }
        match key {
            "name" => fm.name = Some(value.to_string()),
            "description" => fm.description = Some(value.to_string()),
            _ => {}
        }
    }

    // No closing delimiter inside the window: not frontmatter.
    if !closed {
        return Frontmatter::default();
    }
    fm
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
    fn missing_file_degrades_to_default() {
        let fm = read_frontmatter(Path::new("Z:/definitely/not/here/SKILL.md"));
        assert_eq!(fm, Frontmatter::default());
    }
}
