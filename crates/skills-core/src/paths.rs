//! Lexical path normalization helpers.
//!
//! Manifest paths (`target`, `aliases`, `lock-file`) are normalized without
//! touching the filesystem so validation is deterministic and portable.

use std::path::{Path, PathBuf};

/// Lexically normalize a relative path: unify separators to `/`, drop `.`
/// segments, resolve `..` against earlier segments.
///
/// Errors (as human-readable reasons) on empty results, absolute paths and
/// paths escaping the root.
pub fn normalize_rel(input: &str) -> Result<String, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("path is empty".to_string());
    }
    if is_absolute_like(trimmed) {
        return Err(format!("path must be relative, got '{trimmed}'"));
    }
    let mut segments: Vec<&str> = Vec::new();
    for seg in trimmed.split(['/', '\\']) {
        match seg {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() {
                    return Err(format!("path escapes the project root: '{trimmed}'"));
                }
            }
            s => segments.push(s),
        }
    }
    if segments.is_empty() {
        return Err(format!("path is empty after normalization: '{trimmed}'"));
    }
    Ok(segments.join("/"))
}

/// Lexically normalize a *declared donor path* without touching the
/// filesystem. Unlike [`normalize_rel`] (used for `target`/`aliases`/
/// `lock-file`, which must stay inside the project root), a `dir` source is
/// read-only, so this normalizer never fails and deliberately preserves
/// paths that leave the project: absolute prefixes and leading `..` are
/// kept. The result is a stable display / dedup / matching key.
///
/// Rules: trim; unify separators to `/`; drop empty and `.` segments;
/// resolve `..` against a preceding real segment while KEEPING unmatched
/// leading `..` segments (`../../x` stays `../../x`); preserve the absolute
/// prefix — a leading `/` stays (`/a/../b` → `/b`), a Windows drive prefix
/// is kept with the drive letter uppercased (`c:\x\..\y` → `C:/y`), a UNC
/// `//server/share/...` keeps its leading `//`; `..` never pops the absolute
/// prefix itself. Empty or `.`-only input yields `""` (callers treat that as
/// the project root).
pub fn normalize_declared(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let bytes = trimmed.as_bytes();

    // Detect and strip an absolute prefix (checked on both separators).
    let (abs_prefix, rest): (Option<String>, &str) =
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            let drive = (bytes[0] as char).to_ascii_uppercase();
            (Some(format!("{drive}:/")), &trimmed[2..])
        } else if bytes.len() >= 2
            && (bytes[0] == b'/' || bytes[0] == b'\\')
            && (bytes[1] == b'/' || bytes[1] == b'\\')
        {
            (Some("//".to_string()), &trimmed[2..])
        } else if bytes[0] == b'/' || bytes[0] == b'\\' {
            (Some("/".to_string()), &trimmed[1..])
        } else {
            (None, trimmed)
        };
    let is_absolute = abs_prefix.is_some();

    // `segments` never contains `..`; unmatched leading `..` are counted
    // separately (only meaningful for relative paths).
    let mut segments: Vec<&str> = Vec::new();
    let mut leading_dotdot: usize = 0;
    for seg in rest.split(['/', '\\']) {
        match seg {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() && !is_absolute {
                    // Nothing to pop and not confined by a prefix: keep it.
                    leading_dotdot += 1;
                }
                // Absolute with nothing to pop: drop (can't escape the root).
            }
            s => segments.push(s),
        }
    }

    let mut all: Vec<&str> = Vec::with_capacity(leading_dotdot + segments.len());
    all.extend(std::iter::repeat_n("..", leading_dotdot));
    all.extend(segments);
    let body = all.join("/");

    match abs_prefix {
        Some(prefix) => format!("{prefix}{body}"),
        None => body,
    }
}

/// Whether the string looks like an absolute path on any platform
/// (`/x`, `\x`, `C:\x`, `C:/x`, `//server/share`).
pub fn is_absolute_like(p: &str) -> bool {
    let b = p.as_bytes();
    if b.first().is_some_and(|c| *c == b'/' || *c == b'\\') {
        return true;
    }
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// Convert a normalized `/`-separated relative path into a [`PathBuf`] using
/// native separators.
pub fn rel_to_path(rel: &str) -> PathBuf {
    rel.split('/').collect()
}

/// Join a manifest-declared path (may be absolute, may use either separator)
/// onto a base directory.
pub fn join_declared(base: &Path, declared: &str) -> PathBuf {
    if is_absolute_like(declared) {
        PathBuf::from(declared)
    } else {
        // Split manually so `a\b` works on POSIX too.
        let mut out = base.to_path_buf();
        for seg in declared.split(['/', '\\']) {
            match seg {
                "" | "." => {}
                s => out.push(s),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_separators_and_dots() {
        assert_eq!(normalize_rel("./a/b").unwrap(), "a/b");
        assert_eq!(normalize_rel("a\\b\\c").unwrap(), "a/b/c");
        assert_eq!(normalize_rel("a//b/./c").unwrap(), "a/b/c");
        assert_eq!(normalize_rel("a/b/../c").unwrap(), "a/c");
    }

    #[test]
    fn rejects_empty_and_dot_only() {
        assert!(normalize_rel("").is_err());
        assert!(normalize_rel("   ").is_err());
        assert!(normalize_rel(".").is_err());
        assert!(normalize_rel("./").is_err());
    }

    #[test]
    fn rejects_absolute() {
        assert!(normalize_rel("/abs").is_err());
        assert!(normalize_rel("\\abs").is_err());
        assert!(normalize_rel("C:\\abs").is_err());
        assert!(normalize_rel("c:/abs").is_err());
    }

    #[test]
    fn rejects_escaping_root() {
        assert!(normalize_rel("..").is_err());
        assert!(normalize_rel("../x").is_err());
        assert!(normalize_rel("a/../../x").is_err());
    }

    #[test]
    fn normalize_declared_relative_inside_root() {
        assert_eq!(normalize_declared("./a/b"), "a/b");
        assert_eq!(normalize_declared("a\\b\\c"), "a/b/c");
        assert_eq!(normalize_declared("a//b/./c"), "a/b/c");
        assert_eq!(normalize_declared("a/b/../c"), "a/c");
        assert_eq!(normalize_declared("a/../../x"), "../x");
    }

    #[test]
    fn normalize_declared_empty_and_root() {
        assert_eq!(normalize_declared(""), "");
        assert_eq!(normalize_declared("   "), "");
        assert_eq!(normalize_declared("."), "");
        assert_eq!(normalize_declared("./"), "");
        assert_eq!(normalize_declared("a/.."), "");
    }

    #[test]
    fn normalize_declared_keeps_leading_dotdot() {
        assert_eq!(normalize_declared(".."), "..");
        assert_eq!(normalize_declared("../x"), "../x");
        assert_eq!(normalize_declared("..\\x"), "../x");
        assert_eq!(normalize_declared("../../x"), "../../x");
        assert_eq!(normalize_declared("../sibling/skills"), "../sibling/skills");
    }

    #[test]
    fn normalize_declared_preserves_absolute_prefix() {
        assert_eq!(normalize_declared("/a/../b"), "/b");
        assert_eq!(normalize_declared("\\a\\b"), "/a/b");
        // `..` never pops the root prefix.
        assert_eq!(normalize_declared("/.."), "/");
        assert_eq!(normalize_declared("/../a"), "/a");
        assert_eq!(normalize_declared("/opt/skills"), "/opt/skills");
    }

    #[test]
    fn normalize_declared_windows_drive_uppercased() {
        assert_eq!(normalize_declared("c:\\x\\..\\y"), "C:/y");
        assert_eq!(normalize_declared("C:/shared/skills"), "C:/shared/skills");
        assert_eq!(normalize_declared("c:/a/../..\\b"), "C:/b");
    }

    #[test]
    fn normalize_declared_unc_keeps_double_slash() {
        assert_eq!(
            normalize_declared("//server/share/x/../y"),
            "//server/share/y"
        );
        assert_eq!(
            normalize_declared("\\\\server\\share\\a"),
            "//server/share/a"
        );
    }

    #[test]
    fn normalize_declared_dedup_keys_match() {
        assert_eq!(normalize_declared("./a"), normalize_declared("a\\"));
        assert_eq!(normalize_declared("a"), normalize_declared("./a"));
        assert_eq!(normalize_declared("../x"), normalize_declared("..\\x"));
    }

    #[test]
    fn rel_to_path_uses_components() {
        let p = rel_to_path("a/b/c");
        let expected: PathBuf = ["a", "b", "c"].iter().collect();
        assert_eq!(p, expected);
    }

    #[test]
    fn join_declared_handles_both_separators() {
        let base = Path::new("root");
        assert_eq!(
            join_declared(base, "./x/y"),
            Path::new("root").join("x").join("y")
        );
        assert_eq!(
            join_declared(base, "x\\y"),
            Path::new("root").join("x").join("y")
        );
    }

    #[test]
    fn join_declared_keeps_absolute() {
        assert_eq!(
            join_declared(Path::new("root"), "/abs/x"),
            PathBuf::from("/abs/x")
        );
    }
}
