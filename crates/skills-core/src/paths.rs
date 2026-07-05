//! Lexical path normalization helpers.
//!
//! Manifest paths (`target`, `aliases`, `local.dir`) are normalized without
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
