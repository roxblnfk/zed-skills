//! Filesystem helpers: file listing, content hashing, file-set copies.

use std::io;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::paths::rel_to_path;

/// List all files under `dir` as sorted, `/`-separated relative paths.
pub fn list_files(dir: &Path) -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(dir).follow_links(false) {
        let entry = entry.map_err(io::Error::other)?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(dir)
            .map_err(io::Error::other)?
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        out.push(rel);
    }
    out.sort();
    Ok(out)
}

/// SHA-256 over sorted relative paths + file contents.
///
/// Stream format per file (in sorted path order):
/// `path bytes`, `0x00`, `u64-le content length`, `content bytes`.
pub fn content_hash(dir: &Path, files: &[String]) -> io::Result<String> {
    let mut sorted: Vec<&String> = files.iter().collect();
    sorted.sort();
    let mut hasher = Sha256::new();
    for rel in sorted {
        let bytes = std::fs::read(dir.join(rel_to_path(rel)))?;
        hasher.update(rel.as_bytes());
        hasher.update([0u8]);
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    Ok(hex(&hasher.finalize()))
}

/// Copy the given relative files from `src_dir` to `dst_dir`, creating parent
/// directories as needed. Existing destination files are overwritten.
pub fn copy_files(src_dir: &Path, files: &[String], dst_dir: &Path) -> io::Result<()> {
    for rel in files {
        let rel_path = rel_to_path(rel);
        let src = src_dir.join(&rel_path);
        let dst = dst_dir.join(&rel_path);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&src, &dst)?;
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel_to_path(rel));
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    #[test]
    fn list_files_sorted_relative() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "b.txt", "b");
        write(tmp.path(), "a/nested.txt", "n");
        write(tmp.path(), "a.txt", "a");
        let files = list_files(tmp.path()).unwrap();
        assert_eq!(files, vec!["a.txt", "a/nested.txt", "b.txt"]);
    }

    #[test]
    fn content_hash_is_stable() {
        // Pinned expected value: guards the hash stream format against
        // accidental changes (lockfiles in the wild depend on it).
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "SKILL.md", "---\nname: x\n---\n");
        write(tmp.path(), "scripts/run.ps1", "Write-Host hi\n");
        let files = list_files(tmp.path()).unwrap();
        let h1 = content_hash(tmp.path(), &files).unwrap();
        let h2 = content_hash(tmp.path(), &files).unwrap();
        assert_eq!(h1, h2);
        assert_eq!(
            h1,
            "05af098515c77911889ae4ce0285d5c4c9b77b6ee6b8dfe83ae3f206fd4ac473"
        );
    }

    #[test]
    fn content_hash_ignores_input_order() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "a.txt", "a");
        write(tmp.path(), "b.txt", "b");
        let fwd = vec!["a.txt".to_string(), "b.txt".to_string()];
        let rev = vec!["b.txt".to_string(), "a.txt".to_string()];
        assert_eq!(
            content_hash(tmp.path(), &fwd).unwrap(),
            content_hash(tmp.path(), &rev).unwrap()
        );
    }

    #[test]
    fn content_hash_changes_with_content_and_paths() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "a.txt", "a");
        let files = list_files(tmp.path()).unwrap();
        let h1 = content_hash(tmp.path(), &files).unwrap();

        write(tmp.path(), "a.txt", "changed");
        let h2 = content_hash(tmp.path(), &files).unwrap();
        assert_ne!(h1, h2);

        // Same content under a different name hashes differently.
        let tmp2 = tempfile::tempdir().unwrap();
        write(tmp2.path(), "b.txt", "a");
        let files2 = list_files(tmp2.path()).unwrap();
        assert_ne!(h1, content_hash(tmp2.path(), &files2).unwrap());
    }

    #[test]
    fn copy_files_creates_parents_and_overwrites() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        write(src.path(), "a/deep/file.txt", "new");
        write(dst.path(), "a/deep/file.txt", "old");
        copy_files(src.path(), &["a/deep/file.txt".to_string()], dst.path()).unwrap();
        let got = fs::read_to_string(dst.path().join("a").join("deep").join("file.txt")).unwrap();
        assert_eq!(got, "new");
    }
}
