//! Zip archive extraction with zip-slip protection.
//!
//! Every entry name is validated *before* any byte hits disk: absolute
//! paths, drive letters, `..` segments, NUL bytes and backslashes are all
//! rejected. Extraction goes to a temp dir next to the destination; the
//! archive must contain exactly one top-level directory (GitHub zipballs
//! wrap everything in `<owner>-<repo>-<sha>/`, GitLab in
//! `<project>-<ref>-<sha>/`), which is unwrapped and atomically renamed
//! into place (copy fallback for cross-volume moves).

use std::io::{Cursor, Read};
use std::path::Path;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ArchiveError {
    #[error("archive contains an unsafe entry path '{0}' - refusing to extract")]
    UnsafeEntry(String),
    #[error("archive does not contain exactly one top-level directory")]
    TopLevelShape,
    #[error("invalid zip archive: {0}")]
    Zip(String),
    #[error("io error at {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Lexical zip-slip check on a raw zip entry name.
///
/// Rejected: empty names, NUL bytes, backslashes (Windows extractors honor
/// them as separators), absolute paths (`/x`, `\x`), drive letters
/// (`C:` anywhere in the first segment) and any `..` segment.
pub fn is_safe_entry_name(name: &str) -> bool {
    if name.is_empty() || name.contains('\0') || name.contains('\\') {
        return false;
    }
    if name.starts_with('/') {
        return false;
    }
    // Windows drive-letter absolutes: `C:foo`, `C:/foo`.
    let bytes = name.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return false;
    }
    !name.split('/').any(|segment| segment == "..")
}

/// Extract `bytes` (a zip archive) into `dest`, unwrapping the single
/// top-level directory. A pre-existing `dest` (stale partial cache) is
/// removed first. `dest`'s parent is created as needed.
pub fn extract_zip_unwrapped(bytes: &[u8], dest: &Path) -> Result<(), ArchiveError> {
    let io_err = |path: &Path, source: std::io::Error| ArchiveError::Io {
        path: path.to_path_buf(),
        source,
    };

    let mut zip =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| ArchiveError::Zip(e.to_string()))?;

    // Validate every entry name before extracting anything.
    for name in zip.file_names() {
        if !is_safe_entry_name(name) {
            return Err(ArchiveError::UnsafeEntry(name.to_string()));
        }
    }

    let parent = dest.parent().unwrap_or(dest);
    std::fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
    let temp = tempfile::Builder::new()
        .prefix(".skills-extract-")
        .tempdir_in(parent)
        .map_err(|e| io_err(parent, e))?;

    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| ArchiveError::Zip(e.to_string()))?;
        // Names were validated above; build the output path from the raw
        // `/`-separated name.
        let mut out = temp.path().to_path_buf();
        for segment in entry.name().split('/').filter(|s| !s.is_empty()) {
            out.push(segment);
        }
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| io_err(&out, e))?;
            continue;
        }
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
        }
        let mut content = Vec::new();
        entry
            .read_to_end(&mut content)
            .map_err(|e| ArchiveError::Zip(e.to_string()))?;
        std::fs::write(&out, content).map_err(|e| io_err(&out, e))?;
    }

    // Exactly one top-level directory (loose top-level metadata files, if
    // any, are ignored — mirroring the reference implementation).
    let mut top_dirs = Vec::new();
    let listing = std::fs::read_dir(temp.path()).map_err(|e| io_err(temp.path(), e))?;
    for entry in listing {
        let entry = entry.map_err(|e| io_err(temp.path(), e))?;
        if entry.path().is_dir() {
            top_dirs.push(entry.path());
        }
    }
    let [top] = top_dirs.as_slice() else {
        return Err(ArchiveError::TopLevelShape);
    };

    // Stale partial cache is regenerable, never user-owned: drop it.
    if dest.exists() {
        std::fs::remove_dir_all(dest).map_err(|e| io_err(dest, e))?;
    }
    if std::fs::rename(top, dest).is_err() {
        // Cross-volume (or locked) rename: fall back to a recursive copy.
        copy_dir_recursive(top, dest).map_err(|e| io_err(dest, e))?;
    }
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::build_zip;

    #[test]
    fn safe_entry_name_table() {
        let safe = [
            "repo-1.0.0/SKILL.md",
            "repo/skills/a/SKILL.md",
            "a",
            "a/b/",
            "weird name with spaces/x",
            "..hidden/x", // '..' must be a full segment to be rejected
            "a/..b/c",
            "a../b",
        ];
        for name in safe {
            assert!(is_safe_entry_name(name), "expected safe: {name:?}");
        }
        let unsafe_ = [
            "",
            "/etc/passwd",
            "\\windows\\system32",
            "C:/evil",
            "c:evil",
            "Z:\\evil",
            "../escape",
            "a/../../b",
            "a/..",
            "a\\b",
            "nul\0byte",
        ];
        for name in unsafe_ {
            assert!(!is_safe_entry_name(name), "expected unsafe: {name:?}");
        }
    }

    #[test]
    fn extracts_and_unwraps_single_top_level_dir() {
        let bytes = build_zip(&[
            ("repo-abc123/", None),
            ("repo-abc123/SKILL.md", Some("hello")),
            ("repo-abc123/skills/one/SKILL.md", Some("one")),
        ]);
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("cache").join("entry");
        extract_zip_unwrapped(&bytes, &dest).unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.join("SKILL.md")).unwrap(),
            "hello"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("skills").join("one").join("SKILL.md")).unwrap(),
            "one"
        );
        // No temp leftovers next to the destination.
        let leftovers: Vec<_> = std::fs::read_dir(dest.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".skills-extract"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn implicit_directories_work_without_dir_entries() {
        // Some zips omit directory entries entirely.
        let bytes = build_zip(&[("top/deep/nested/file.txt", Some("x"))]);
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("out");
        extract_zip_unwrapped(&bytes, &dest).unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.join("deep").join("nested").join("file.txt")).unwrap(),
            "x"
        );
    }

    #[test]
    fn stale_partial_dest_is_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("entry");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("stale.txt"), "old").unwrap();

        let bytes = build_zip(&[("top/fresh.txt", Some("new"))]);
        extract_zip_unwrapped(&bytes, &dest).unwrap();
        assert!(!dest.join("stale.txt").exists());
        assert_eq!(
            std::fs::read_to_string(dest.join("fresh.txt")).unwrap(),
            "new"
        );
    }

    #[test]
    fn traversal_entry_rejected_without_writing() {
        let bytes = build_zip(&[
            ("top/ok.txt", Some("fine")),
            ("top/../../evil.txt", Some("boom")),
        ]);
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("sub").join("entry");
        let err = extract_zip_unwrapped(&bytes, &dest).unwrap_err();
        assert!(matches!(err, ArchiveError::UnsafeEntry(_)), "{err}");
        // Nothing extracted at all (validation precedes extraction).
        assert!(!dest.exists());
        assert!(!tmp.path().join("evil.txt").exists());
    }

    #[test]
    fn absolute_drive_letter_and_backslash_entries_rejected() {
        for name in ["/abs.txt", "C:/evil.txt", "top\\win.txt"] {
            let bytes = build_zip(&[(name, Some("x"))]);
            let tmp = tempfile::tempdir().unwrap();
            let err = extract_zip_unwrapped(&bytes, &tmp.path().join("e")).unwrap_err();
            assert!(matches!(err, ArchiveError::UnsafeEntry(_)), "{name}: {err}");
        }
    }

    #[test]
    fn multiple_top_level_dirs_rejected() {
        let bytes = build_zip(&[("one/a.txt", Some("1")), ("two/b.txt", Some("2"))]);
        let tmp = tempfile::tempdir().unwrap();
        let err = extract_zip_unwrapped(&bytes, &tmp.path().join("e")).unwrap_err();
        assert!(matches!(err, ArchiveError::TopLevelShape), "{err}");
    }

    #[test]
    fn no_top_level_dir_rejected() {
        let bytes = build_zip(&[("loose.txt", Some("x"))]);
        let tmp = tempfile::tempdir().unwrap();
        let err = extract_zip_unwrapped(&bytes, &tmp.path().join("e")).unwrap_err();
        assert!(matches!(err, ArchiveError::TopLevelShape), "{err}");
    }

    #[test]
    fn loose_top_level_files_are_tolerated_beside_the_single_dir() {
        let bytes = build_zip(&[
            ("pax_global_header", Some("meta")),
            ("repo/SKILL.md", Some("s")),
        ]);
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("e");
        extract_zip_unwrapped(&bytes, &dest).unwrap();
        assert!(dest.join("SKILL.md").is_file());
    }

    #[test]
    fn garbage_bytes_are_a_zip_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = extract_zip_unwrapped(b"not a zip", &tmp.path().join("e")).unwrap_err();
        assert!(matches!(err, ArchiveError::Zip(_)), "{err}");
    }
}
