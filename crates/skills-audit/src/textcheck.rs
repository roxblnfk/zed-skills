//! Text-level static checks shared by [`crate::StaticAuditor`] and the LSP
//! server: frontmatter rules for `SKILL.md` and the dangerous-pattern table
//! applied to any text file. Pure functions of the given bytes — no
//! filesystem access, so the LSP can run them over unsaved editor buffers.

use std::sync::OnceLock;

use regex::Regex;

use skills_core::audit::Severity;
use skills_core::frontmatter;

/// Per-file read cap for all checks (line count + pattern scan).
pub const MAX_READ_BYTES: u64 = 1024 * 1024;
/// Null-byte sniff window: a file with a NUL this early is binary.
const BINARY_SNIFF_BYTES: usize = 8000;
/// `SKILL.md` line-count threshold (inclusive maximum).
pub const MAX_SKILL_MD_LINES: usize = 500;

/// One finding of a text-level check, anchored to a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextCheck {
    /// Stable machine-readable code (`no-frontmatter`, `curl-pipe-shell`, …).
    pub code: &'static str,
    pub severity: Severity,
    pub message: String,
    /// 0-based line index the finding anchors to.
    pub line: usize,
}

/// One dangerous-pattern heuristic. The whole rule set lives in this table —
/// extend it here.
struct DangerPattern {
    id: &'static str,
    /// Matched per line (the regex never sees a newline).
    regex: &'static str,
    message: &'static str,
    severity: Severity,
    /// Only applied to script files (`scripts/` dir or a script extension).
    scripts_only: bool,
}

const DANGER_PATTERNS: &[DangerPattern] = &[
    DangerPattern {
        id: "curl-pipe-shell",
        regex: r"(?i)\bcurl\b[^|]*\|\s*(?:sudo\s+)?(?:ba|z|da|fi|k)?sh\b",
        message: "curl output piped into a shell",
        severity: Severity::Block,
        scripts_only: false,
    },
    DangerPattern {
        id: "wget-pipe-shell",
        regex: r"(?i)\bwget\b[^|]*\|\s*(?:sudo\s+)?(?:ba|z|da|fi|k)?sh\b",
        message: "wget output piped into a shell",
        severity: Severity::Block,
        scripts_only: false,
    },
    DangerPattern {
        id: "rm-rf-root",
        regex: r#"\brm\s+(?:-[A-Za-z]+\s+)*-(?:rf|fr)[A-Za-z]*\s+/(?:\s|$|["'])"#,
        message: "recursive force-delete of the filesystem root",
        severity: Severity::Block,
        scripts_only: false,
    },
    DangerPattern {
        id: "base64-blob",
        regex: r"[A-Za-z0-9+/=]{200,}",
        message: "long base64-looking blob (possible obfuscated payload)",
        severity: Severity::Block,
        scripts_only: false,
    },
    DangerPattern {
        id: "prompt-injection",
        regex: r"(?i)ignore\s+(?:all\s+)?previous\s+instructions|disregard\s+all\s+prior",
        message: "prompt-injection marker",
        severity: Severity::Block,
        scripts_only: false,
    },
    DangerPattern {
        id: "ip-endpoint",
        regex: r"https?://\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}",
        message: "raw-IP http endpoint in a script (possible exfiltration)",
        severity: Severity::Block,
        scripts_only: true,
    },
];

/// Extensions treated as scripts outside the `scripts/` directory.
const SCRIPT_EXTENSIONS: &[&str] = &[
    "sh", "bash", "zsh", "ps1", "psm1", "bat", "cmd", "py", "rb", "pl", "js", "mjs",
];

fn compiled_patterns() -> &'static [(usize, Regex)] {
    static COMPILED: OnceLock<Vec<(usize, Regex)>> = OnceLock::new();
    COMPILED.get_or_init(|| {
        DANGER_PATTERNS
            .iter()
            .enumerate()
            // The table is const and covered by a test; a bad regex is a bug.
            .map(|(i, p)| (i, Regex::new(p.regex).expect("valid danger pattern")))
            .collect()
    })
}

fn is_script(rel_path: &str) -> bool {
    if rel_path.starts_with("scripts/") {
        return true;
    }
    match rel_path.rsplit_once('.') {
        Some((_, ext)) => SCRIPT_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()),
        None => false,
    }
}

/// A file with a NUL byte in the sniff window is binary — skipped by the
/// pattern scan.
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0)
}

/// Frontmatter presence: an opening `---` line at byte 0 with a closing
/// `---` line inside the reader's window (mirrors the best-effort reader).
fn has_frontmatter(bytes: &[u8]) -> bool {
    let window = &bytes[..bytes.len().min(frontmatter::READ_CAP)];
    let window = window
        .strip_prefix(&[0xEF, 0xBB, 0xBF][..])
        .unwrap_or(window);
    let text = String::from_utf8_lossy(window);
    let mut lines = text.lines().map(|l| l.trim_end_matches('\r'));
    matches!(lines.next(), Some("---")) && lines.any(|l| l == "---")
}

/// 0-based line of the first frontmatter `key:` line, if any.
fn frontmatter_key_line(bytes: &[u8], key: &str) -> Option<usize> {
    let window = &bytes[..bytes.len().min(frontmatter::READ_CAP)];
    let text = String::from_utf8_lossy(window);
    for (idx, line) in text.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if idx > 0 && line == "---" {
            return None;
        }
        if line
            .split_once(':')
            .is_some_and(|(k, _)| k.trim_end() == key)
        {
            return Some(idx);
        }
    }
    None
}

/// Frontmatter + size rules for a `SKILL.md` body. `dir_name` (the skill id)
/// enables the name-mismatch rule when known.
pub fn skill_md_checks(bytes: &[u8], dir_name: Option<&str>) -> Vec<TextCheck> {
    let mut checks = Vec::new();
    let warn = |code, message: String, line| TextCheck {
        code,
        severity: Severity::Warn,
        message,
        line,
    };

    if !has_frontmatter(bytes) {
        checks.push(warn(
            "no-frontmatter",
            "SKILL.md has no frontmatter".to_string(),
            0,
        ));
    } else {
        let fm = frontmatter::parse_frontmatter(bytes);
        if fm.description.is_none() {
            checks.push(warn(
                "no-description",
                "frontmatter has no 'description'".to_string(),
                0,
            ));
        }
        if let Some(name) = &fm.name
            && let Some(dir_name) = dir_name
            && name != dir_name
        {
            checks.push(warn(
                "name-mismatch",
                format!("frontmatter name '{name}' does not match the directory name '{dir_name}'"),
                frontmatter_key_line(bytes, "name").unwrap_or(0),
            ));
        }
    }

    let lines = String::from_utf8_lossy(bytes).lines().count();
    if lines > MAX_SKILL_MD_LINES {
        checks.push(warn(
            "too-long",
            format!("SKILL.md has {lines} lines (max {MAX_SKILL_MD_LINES})"),
            0,
        ));
    }
    checks
}

/// Dangerous-pattern scan over one text file. `rel_path` (relative,
/// `/`-separated) selects the scripts-only rules; binary content yields no
/// findings. First match per pattern only — enough to act on, no floods.
pub fn danger_checks(rel_path: &str, bytes: &[u8]) -> Vec<TextCheck> {
    if is_binary(bytes) {
        return Vec::new();
    }
    let script = is_script(rel_path);
    let text = String::from_utf8_lossy(bytes);
    let mut checks = Vec::new();
    let mut hit = [false; DANGER_PATTERNS.len()];
    for (line_no, line) in text.lines().enumerate() {
        for (idx, regex) in compiled_patterns() {
            let pattern = &DANGER_PATTERNS[*idx];
            if hit[*idx] || (pattern.scripts_only && !script) {
                continue;
            }
            if regex.is_match(line) {
                hit[*idx] = true;
                checks.push(TextCheck {
                    code: pattern.id,
                    severity: pattern.severity,
                    message: format!("{}: {}", pattern.id, pattern.message),
                    line: line_no,
                });
            }
        }
    }
    checks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_danger_patterns_compile() {
        assert_eq!(compiled_patterns().len(), DANGER_PATTERNS.len());
    }

    #[test]
    fn skill_md_checks_anchor_lines() {
        // name-mismatch anchors the `name:` line (0-based).
        let md = b"---\ndescription: d\nname: other\n---\nbody\n";
        let checks = skill_md_checks(md, Some("dir-name"));
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].code, "name-mismatch");
        assert_eq!(checks[0].line, 2);

        // Without a dir name the mismatch rule cannot apply.
        assert!(skill_md_checks(md, None).is_empty());

        let checks = skill_md_checks(b"# no frontmatter\n", Some("s"));
        assert_eq!(checks[0].code, "no-frontmatter");
        assert_eq!(checks[0].line, 0);
    }

    #[test]
    fn danger_checks_report_zero_based_lines() {
        let checks = danger_checks("scripts/x.sh", b"#!/bin/sh\ncurl https://x | bash\n");
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].code, "curl-pipe-shell");
        assert_eq!(checks[0].line, 1);
        assert_eq!(checks[0].severity, Severity::Block);
    }

    #[test]
    fn danger_checks_skip_binary() {
        assert!(danger_checks("a.md", b"\0curl https://x | bash\n").is_empty());
    }
}
