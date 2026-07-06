//! Directory-level alias linking: one OS link per alias path → target.
//!
//! Ported from the PHP `SymlinkLinker`. Aliases mirror the sync target
//! (`.agents/skills`) into other well-known locations (`.claude/skills`, …).
//! One alias == one link, created *after* the copy step so the target
//! directory already exists.
//!
//! Platform split (cfg-gated [`create_link`]):
//! - **Windows**: NTFS **junctions** via `cmd /c mklink /J`. Junctions need
//!   no admin / developer mode (unlike symbolic links, which require the
//!   `SeCreateSymbolicLink` privilege). They are local-FS-only — a
//!   cross-volume alias is reported as [`LinkStatus::Failed`], never
//!   silently degraded to a copy.
//! - **POSIX**: `std::os::unix::fs::symlink`.
//!
//! The state matrix is applied to the alias path *before* creating anything;
//! the linker never destroys user-owned content (no "force" mode).

use std::path::Path;

/// Discrete outcome of an alias-link attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkStatus {
    /// A new junction / symlink was put in place.
    Created,
    /// The alias path already pointed at the target; no-op success.
    AlreadyCorrect,
    /// Dry-run: nothing was written, but the linker would have created it.
    WouldCreate,
    /// State-matrix rejection (alias occupied, points elsewhere, cross-volume
    /// on Windows, missing target, …). `reason` is set.
    Failed,
}

/// Outcome of a single [`link`] call. FS-level failures never panic — each
/// alias is independent, and the caller decides policy (warn vs. fail the
/// run) once the whole list has been processed.
#[derive(Debug, Clone)]
pub struct LinkOutcome {
    pub status: LinkStatus,
    /// Human-readable failure detail; `Some` only when `status == Failed`.
    pub reason: Option<String>,
}

impl LinkOutcome {
    fn ok(status: LinkStatus) -> Self {
        LinkOutcome {
            status,
            reason: None,
        }
    }

    fn failed(reason: impl Into<String>) -> Self {
        LinkOutcome {
            status: LinkStatus::Failed,
            reason: Some(reason.into()),
        }
    }

    pub fn is_failure(&self) -> bool {
        self.status == LinkStatus::Failed
    }
}

/// Make `alias` point at `target`.
///
/// `target` is assumed to exist (callers run this after the copy step). In
/// `dry_run` mode every read-only check still runs, but nothing is written:
/// a would-be-created alias is reported as [`LinkStatus::WouldCreate`], while
/// state-matrix collisions are reported identically to a normal run.
pub fn link(alias: &Path, target: &Path, dry_run: bool) -> LinkOutcome {
    // Resolve the target for the "points at target?" comparison. In dry-run
    // the target may not exist yet (the copy step did not write it); fall
    // back to the literal path so collisions can still be reported.
    let resolved_target = match std::fs::canonicalize(target) {
        Ok(p) => p,
        Err(_) => {
            if !dry_run {
                return LinkOutcome::failed(format!(
                    "target directory does not exist: {}",
                    target.display()
                ));
            }
            target.to_path_buf()
        }
    };

    // Inspect the alias path without following it (junctions and symlinks
    // must not be traversed here).
    match std::fs::symlink_metadata(alias) {
        Ok(meta) => return handle_existing(alias, &resolved_target, &meta),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return LinkOutcome::failed(format!(
                "cannot inspect alias path {}: {e}",
                alias.display()
            ));
        }
    }

    // Cross-volume junctions are impossible on Windows — detect before we
    // touch the filesystem.
    #[cfg(windows)]
    if cross_volume(alias, &resolved_target) {
        return LinkOutcome::failed(
            "cross-volume junctions are not supported on Windows \
             (target is on a different drive or share)",
        );
    }

    if dry_run {
        return LinkOutcome::ok(LinkStatus::WouldCreate);
    }

    // Create the alias parent if needed (the target's copy step does the same
    // for the target itself).
    if let Some(parent) = alias.parent()
        && !parent.as_os_str().is_empty()
        && !parent.is_dir()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return LinkOutcome::failed(format!(
            "failed to create parent directory {}: {e}",
            parent.display()
        ));
    }

    // Link against the plain (non-canonicalized) target: `canonicalize` on
    // Windows returns a `\\?\` verbatim path that `mklink` rejects.
    match create_link(alias, target) {
        Ok(()) => LinkOutcome::ok(LinkStatus::Created),
        Err(reason) => LinkOutcome::failed(reason),
    }
}

/// The alias path already exists — classify it against the state matrix.
fn handle_existing(alias: &Path, resolved_target: &Path, meta: &std::fs::Metadata) -> LinkOutcome {
    // Rust classifies both symbolic links and NTFS junctions (mount-point
    // reparse points are name-surrogates) as `is_symlink()`, so this single
    // check covers junctions without the realpath-escape heuristic the PHP
    // version needs.
    if meta.file_type().is_symlink() {
        return match std::fs::canonicalize(alias) {
            Ok(existing) if paths_equal(&existing, resolved_target) => {
                LinkOutcome::ok(LinkStatus::AlreadyCorrect)
            }
            Ok(existing) => LinkOutcome::failed(format!(
                "existing link points elsewhere ({}); refusing to overwrite",
                existing.display()
            )),
            Err(_) => {
                LinkOutcome::failed("existing link at alias path cannot be resolved (broken link?)")
            }
        };
    }

    if meta.is_dir() {
        return LinkOutcome::failed(
            "a real directory already exists at the alias path; refusing to replace it",
        );
    }

    LinkOutcome::failed("a regular file already exists at the alias path")
}

/// Compare two resolved paths, case-insensitively on Windows.
fn paths_equal(a: &Path, b: &Path) -> bool {
    let a = a.to_string_lossy();
    let a = a.trim_end_matches(['/', '\\']);
    let b = b.to_string_lossy();
    let b = b.trim_end_matches(['/', '\\']);
    #[cfg(windows)]
    {
        a.eq_ignore_ascii_case(b)
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

#[cfg(windows)]
fn create_link(alias: &Path, target: &Path) -> Result<(), String> {
    use std::process::Command;

    // `mklink` is a cmd.exe builtin, so it must be invoked via `cmd /C`.
    // `/J` creates a directory junction (no elevation required).
    let output = Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(alias)
        .arg(target)
        .output()
        .map_err(|e| format!("failed to spawn mklink: {e}"))?;

    if output.status.success() {
        return Ok(());
    }

    let mut detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if detail.is_empty() {
        detail = String::from_utf8_lossy(&output.stdout).trim().to_string();
    }
    if detail.is_empty() {
        detail = "no output".to_string();
    }
    Err(format!("mklink /J failed: {detail}"))
}

#[cfg(unix)]
fn create_link(alias: &Path, target: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, alias).map_err(|e| format!("symlink() failed: {e}"))
}

/// Whether `alias` and `target` live on different Windows volumes (drive
/// letters or UNC shares). A junction cannot span volumes.
#[cfg(windows)]
fn cross_volume(alias: &Path, target: &Path) -> bool {
    match (volume_of(alias), volume_of(target)) {
        (Some(a), Some(b)) => !a.eq_ignore_ascii_case(&b),
        _ => false,
    }
}

/// Extract the volume prefix of a Windows path: a drive letter (`C:`) or a
/// UNC share root (`\\server\share`). `None` when neither is present.
#[cfg(windows)]
fn volume_of(path: &Path) -> Option<String> {
    let raw = path.to_string_lossy().replace('/', "\\");
    let s = raw.strip_prefix(r"\\?\").unwrap_or(&raw);
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Some(s[..2].to_string());
    }
    if let Some(rest) = s.strip_prefix(r"\\") {
        let mut parts = rest.split('\\');
        if let (Some(server), Some(share)) = (parts.next(), parts.next())
            && !server.is_empty()
            && !share.is_empty()
        {
            return Some(format!(r"\\{server}\{share}"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Fixture {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        target: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let target = root.join("target");
        std::fs::create_dir_all(&target).unwrap();
        Fixture {
            _tmp: tmp,
            root,
            target,
        }
    }

    /// Behavioural check: does `path` resolve on disk to the same canonical
    /// location as `target`? Works across symlinks, junctions and platform
    /// separators — no `is_symlink`/`readlink` quirks.
    fn resolves_to(path: &Path, target: &Path) -> bool {
        match (std::fs::canonicalize(path), std::fs::canonicalize(target)) {
            (Ok(a), Ok(b)) => paths_equal(&a, &b),
            _ => false,
        }
    }

    #[test]
    fn creates_link_when_alias_absent() {
        let f = fixture();
        let alias = f.root.join("alias");
        let out = link(&alias, &f.target, false);
        assert_eq!(out.status, LinkStatus::Created, "{:?}", out.reason);
        assert!(alias.exists());
        assert!(resolves_to(&alias, &f.target));
    }

    #[test]
    fn link_resolves_to_target_contents() {
        // A file written into the target must be visible through the alias:
        // proves the link (junction on Windows, symlink on POSIX) points
        // where we claimed — created without elevation.
        let f = fixture();
        let alias = f.root.join("alias");
        assert_eq!(link(&alias, &f.target, false).status, LinkStatus::Created);
        std::fs::write(f.target.join("marker.txt"), "hello").unwrap();
        assert_eq!(
            std::fs::read_to_string(alias.join("marker.txt")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn existing_link_pointing_at_target_is_no_op() {
        let f = fixture();
        let alias = f.root.join("alias");
        assert_eq!(link(&alias, &f.target, false).status, LinkStatus::Created);
        let second = link(&alias, &f.target, false);
        assert_eq!(second.status, LinkStatus::AlreadyCorrect);
    }

    #[test]
    fn existing_link_pointing_elsewhere_fails() {
        let f = fixture();
        let other = f.root.join("other");
        std::fs::create_dir_all(&other).unwrap();
        let alias = f.root.join("alias");
        assert_eq!(link(&alias, &other, false).status, LinkStatus::Created);
        let out = link(&alias, &f.target, false);
        assert_eq!(out.status, LinkStatus::Failed);
        assert!(
            out.reason.as_deref().unwrap().contains("points elsewhere"),
            "{:?}",
            out.reason
        );
    }

    #[test]
    fn existing_real_directory_fails_and_is_untouched() {
        let f = fixture();
        let alias = f.root.join("alias");
        std::fs::create_dir_all(&alias).unwrap();
        std::fs::write(alias.join("user.txt"), "precious").unwrap();
        let out = link(&alias, &f.target, false);
        assert_eq!(out.status, LinkStatus::Failed);
        assert!(alias.is_dir());
        assert_eq!(
            std::fs::read_to_string(alias.join("user.txt")).unwrap(),
            "precious"
        );
    }

    #[test]
    fn existing_regular_file_fails() {
        let f = fixture();
        let alias = f.root.join("alias");
        std::fs::write(&alias, "i am a file").unwrap();
        let out = link(&alias, &f.target, false);
        assert_eq!(out.status, LinkStatus::Failed);
        assert!(alias.is_file());
    }

    #[test]
    fn missing_target_fails_in_real_run() {
        let f = fixture();
        let out = link(&f.root.join("alias"), &f.root.join("does-not-exist"), false);
        assert_eq!(out.status, LinkStatus::Failed);
        assert!(
            out.reason
                .as_deref()
                .unwrap()
                .contains("target directory does not exist"),
            "{:?}",
            out.reason
        );
    }

    #[test]
    fn dry_run_reports_would_create_without_writing() {
        let f = fixture();
        let alias = f.root.join("alias");
        let out = link(&alias, &f.target, true);
        assert_eq!(out.status, LinkStatus::WouldCreate);
        assert!(!alias.exists());
    }

    #[test]
    fn dry_run_still_reports_directory_collision() {
        let f = fixture();
        let alias = f.root.join("alias");
        std::fs::create_dir_all(&alias).unwrap();
        let out = link(&alias, &f.target, true);
        assert_eq!(out.status, LinkStatus::Failed);
    }

    #[test]
    fn creates_missing_parent_directory() {
        let f = fixture();
        let alias = f.root.join("nested").join("parent").join("alias");
        let out = link(&alias, &f.target, false);
        assert_eq!(out.status, LinkStatus::Created, "{:?}", out.reason);
        assert!(f.root.join("nested").join("parent").is_dir());
        assert!(resolves_to(&alias, &f.target));
    }
}
