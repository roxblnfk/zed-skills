//! Skill directory-name safety: FS-dangerous names and the normalized
//! conflict-detection key.
//!
//! A skill's directory name ([`crate::domain::SkillId`]) becomes a real
//! directory under the user's sync target on *every* platform the project is
//! checked out on, so both checks here are deliberately OS-independent
//! portability guarantees, not host checks: a manifest synced on Linux must
//! not produce a lockfile/target a Windows or macOS checkout cannot
//! represent. Pure string logic, no filesystem access.

use unicode_normalization::UnicodeNormalization;

/// Windows reserved device names. Dangerous bare or with any extension
/// (`NUL`, `nul.txt`, `Con.tar.gz`), case-insensitive.
const RESERVED_DEVICE_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Characters Windows filesystems reject in file names.
const ILLEGAL_CHARS: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Why `name` cannot become a directory on every supported filesystem, if
/// any reason applies. The reason reads as a predicate of the name
/// (`"skill directory name 'nul' {reason}"`).
///
/// Checked regardless of the host OS — see the module docs. Control-char
/// coverage is slightly stricter than Windows' own rule (which rejects only
/// U+0000–U+001F): all of [`char::is_control`] is dangerous, since DEL and
/// C1 controls are unrepresentable in most tooling even where the
/// filesystem accepts them.
pub fn dir_name_danger(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("is empty".to_string());
    }
    if let Some(c) = name.chars().find(|c| c.is_control()) {
        return Some(format!("contains a control character (U+{:04X})", c as u32));
    }
    if let Some(c) = name.chars().find(|c| ILLEGAL_CHARS.contains(c)) {
        return Some(format!(
            "contains '{c}', which is illegal in Windows file names"
        ));
    }
    if name.ends_with('.') {
        return Some("ends with a dot, which Windows strips from file names".to_string());
    }
    if name.ends_with(' ') {
        return Some("ends with a space, which Windows strips from file names".to_string());
    }
    // Reserved device names apply to the part before the first dot ("the
    // file name apart from its extension"); trailing spaces in that stem
    // are ignored by Windows too ("nul .txt").
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    if let Some(device) = RESERVED_DEVICE_NAMES
        .iter()
        .find(|d| stem.eq_ignore_ascii_case(d))
    {
        return Some(format!("is the reserved Windows device name '{device}'"));
    }
    None
}

/// Grouping key for conflict detection: Unicode NFC + full lowercase
/// mapping (+ re-NFC, as lowercasing can denormalize). Names that would
/// merge into one directory on a case-insensitive / normalizing filesystem
/// (Windows, macOS) map to the same key: `Foo`, `foo` and NFD-`foo` group
/// together. Only the grouping key is normalized — reports, the lockfile
/// and the target keep the original spellings.
///
/// `char::to_lowercase` is the full Unicode lowercase mapping, not full
/// case folding: locale-free and dependency-free, at the cost of missing
/// exotic folds (e.g. `STRASSE` vs `straße` stay distinct) — acceptable,
/// since NTFS/APFS case-insensitivity is an uppercase/lowercase mapping,
/// not a full fold either.
pub fn conflict_key(name: &str) -> String {
    name.nfc().flat_map(char::to_lowercase).nfc().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_device_names_bare_extension_and_case_variants() {
        for device in RESERVED_DEVICE_NAMES {
            let lower = device.to_ascii_lowercase();
            let mixed: String = device
                .chars()
                .enumerate()
                .map(|(i, c)| {
                    if i % 2 == 0 {
                        c.to_ascii_lowercase()
                    } else {
                        c
                    }
                })
                .collect();
            for name in [
                device.to_string(),
                lower.clone(),
                mixed,
                format!("{device}.txt"),
                format!("{lower}.tar.gz"),
                format!("{lower} .txt"), // stem trailing space ignored by Windows
            ] {
                let reason =
                    dir_name_danger(&name).unwrap_or_else(|| panic!("'{name}' must be dangerous"));
                assert!(
                    reason.contains(&format!("'{device}'")),
                    "'{name}': {reason}"
                );
            }
        }
    }

    #[test]
    fn near_reserved_names_are_fine() {
        for name in [
            "console", "nulx", "aux2", "com10", "com0", "lpt", "lpt10", "prn2", "co", "nu.l",
        ] {
            assert_eq!(dir_name_danger(name), None, "'{name}' must be safe");
        }
    }

    #[test]
    fn trailing_dot_and_space_are_dangerous() {
        for name in ["foo.", "foo ", "foo..", ".", "..", "trailing. "] {
            let reason =
                dir_name_danger(name).unwrap_or_else(|| panic!("'{name}' must be dangerous"));
            assert!(
                reason.contains("ends with"),
                "'{name}': {reason} (expected a trailing-char reason)"
            );
        }
        // Leading/inner dots are fine.
        assert_eq!(dir_name_danger(".hidden"), None);
        assert_eq!(dir_name_danger("skill.v2"), None);
    }

    #[test]
    fn each_illegal_char_is_dangerous() {
        for c in ['<', '>', ':', '"', '/', '\\', '|', '?', '*'] {
            let name = format!("bad{c}name");
            let reason =
                dir_name_danger(&name).unwrap_or_else(|| panic!("'{name}' must be dangerous"));
            assert!(reason.contains(&format!("'{c}'")), "{reason}");
        }
    }

    #[test]
    fn control_chars_are_dangerous() {
        for (c, code) in [('\u{1}', "U+0001"), ('\u{7F}', "U+007F"), ('\n', "U+000A")] {
            let reason = dir_name_danger(&format!("bad{c}name")).expect("dangerous");
            assert!(reason.contains(code), "{reason}");
        }
    }

    #[test]
    fn empty_name_is_dangerous_clean_names_pass() {
        assert!(dir_name_danger("").is_some());
        for name in [
            "code-review",
            "Foo", // tier-1-safe; only the spec rules dislike the case
            "skill.v2",
            "über-skill", // non-ASCII is FS-safe
            "a b",        // inner space is FS-safe
        ] {
            assert_eq!(dir_name_danger(name), None, "'{name}' must be safe");
        }
    }

    #[test]
    fn conflict_key_folds_case() {
        assert_eq!(conflict_key("Foo"), conflict_key("foo"));
        assert_eq!(conflict_key("CODE-Review"), conflict_key("code-review"));
        assert_ne!(conflict_key("foo"), conflict_key("bar"));
    }

    #[test]
    fn conflict_key_normalizes_unicode() {
        // "café": NFC (U+00E9) vs NFD (e + U+0301 combining acute).
        assert_eq!(conflict_key("caf\u{e9}"), conflict_key("cafe\u{301}"));
        // Case + normalization combined.
        assert_eq!(conflict_key("CAF\u{c9}"), conflict_key("cafe\u{301}"));
    }

    #[test]
    fn conflict_key_keeps_ascii_verbatim() {
        assert_eq!(conflict_key("code-review"), "code-review");
    }
}
