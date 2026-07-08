//! Filesystem watcher: the LSP server watches its inputs itself (`notify`
//! with the `ReadDirectoryChangesW` backend on Windows) instead of relying
//! on client-side `workspace/didChangeWatchedFiles`.
//!
//! Watch set: the project root (non-recursive — covers `skills.json` and a
//! root-level lockfile), every `sources[]` dir-entry path, `vendor/`
//! (composer donors), the sync target and the parent dir of a non-root
//! `lock-file` path.
//! Events are debounced by `notify-debouncer-mini` and
//! forwarded as a unit signal; the server re-analyzes open documents and
//! re-resolves the watch set (the manifest may have changed).

use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_mini::{DebounceEventResult, Debouncer, new_debouncer};

use skills_core::manifest::Manifest;
use skills_core::paths::{join_declared, rel_to_path};

/// Debounce window for raw FS events.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// A running watcher. Dropping it stops the watcher thread and releases its
/// event-channel sender — with all handles dropped, the server's event loop
/// task ends too (no orphan threads after shutdown).
pub struct WatchHandle {
    _debouncer: Debouncer<notify::RecommendedWatcher>,
}

/// Start watching `project_root` (and the manifest-derived path set),
/// signalling debounced changes through `tx`. The signal payload is `true`
/// when `skills.json` itself changed — the receiver then re-resolves the
/// watch set.
pub fn start(
    project_root: &Path,
    manifest: Option<&Manifest>,
    tx: tokio::sync::mpsc::UnboundedSender<bool>,
) -> notify::Result<WatchHandle> {
    let mut debouncer = new_debouncer(DEBOUNCE, move |result: DebounceEventResult| {
        if let Ok(events) = result {
            let manifest_changed = events.iter().any(|event| {
                event.path.file_name().is_some_and(|name| {
                    name.eq_ignore_ascii_case(skills_core::manifest::MANIFEST_NAME)
                })
            });
            let _ = tx.send(manifest_changed);
        }
    })?;

    // The root itself must exist — everything else is best-effort (paths may
    // not have been created yet).
    debouncer
        .watcher()
        .watch(project_root, RecursiveMode::NonRecursive)?;
    for path in watch_set(project_root, manifest) {
        let _ = debouncer.watcher().watch(&path, RecursiveMode::Recursive);
    }

    Ok(WatchHandle {
        _debouncer: debouncer,
    })
}

/// The recursive watch paths derived from the manifest.
fn watch_set(project_root: &Path, manifest: Option<&Manifest>) -> Vec<PathBuf> {
    let mut paths = vec![project_root.join("vendor")];
    if let Some(manifest) = manifest {
        // Every dir-entry donor path (their skills feed the analysis).
        for entry in manifest.sources().iter().filter(|e| e.from == "dir") {
            if let Some(path) = entry.path.as_deref() {
                paths.push(join_declared(project_root, path));
            }
        }
        paths.push(project_root.join(rel_to_path(&manifest.effective_target())));
        let lock_abs = project_root.join(rel_to_path(&manifest.effective_lock_file()));
        if let Some(parent) = lock_abs.parent()
            && parent != project_root
            && !paths.iter().any(|p| p == parent)
        {
            paths.push(parent.to_path_buf());
        }
    }
    paths.retain(|p| p.is_dir());
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_set_keeps_only_existing_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("skills-src")).unwrap();
        std::fs::create_dir_all(tmp.path().join("vendor")).unwrap();
        let manifest = Manifest::parse(
            r#"{ "sources": [
                { "from": "dir", "path": "./skills-src" },
                { "from": "dir", "path": "./missing" }
            ], "target": "t" }"#,
        )
        .unwrap();
        let set = watch_set(tmp.path(), Some(&manifest));
        assert_eq!(
            set,
            vec![tmp.path().join("vendor"), tmp.path().join("skills-src")]
        );
    }

    #[test]
    fn watch_set_includes_outward_and_absolute_dir_paths() {
        // A dir source may be outward (`../x`) or absolute — both legal reads.
        // They register in the watch set when they exist on disk (no in-root
        // filtering).
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("project");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::create_dir_all(tmp.path().join("sibling")).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let abs = outside.path().join("shared");
        std::fs::create_dir_all(&abs).unwrap();

        let manifest = Manifest::parse(
            &serde_json::json!({
                "sources": [
                    { "from": "dir", "path": "../sibling" },
                    { "from": "dir", "path": abs.to_string_lossy() }
                ],
                "target": "t"
            })
            .to_string(),
        )
        .unwrap();
        let set = watch_set(&proj, Some(&manifest));
        assert!(
            set.contains(&join_declared(&proj, "../sibling")),
            "outward path missing: {set:?}"
        );
        assert!(set.contains(&abs), "absolute path missing: {set:?}");
    }

    #[test]
    fn watch_set_includes_non_root_lock_file_parent() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".meta")).unwrap();
        let manifest =
            Manifest::parse(r#"{ "target": "t", "lock-file": ".meta/skills.lock" }"#).unwrap();
        let set = watch_set(tmp.path(), Some(&manifest));
        assert_eq!(set, vec![tmp.path().join(".meta")]);

        // Root-level lock (the default) adds nothing: the non-recursive root
        // watch already covers it.
        let manifest = Manifest::parse(r#"{ "target": "t" }"#).unwrap();
        assert!(watch_set(tmp.path(), Some(&manifest)).is_empty());
    }

    #[tokio::test]
    async fn watcher_signals_on_change_and_stops_on_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = start(tmp.path(), None, tx).unwrap();

        std::fs::write(tmp.path().join("skills.lock"), "{}").unwrap();
        let signal = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await;
        // skills.lock is not the manifest → payload is `false`.
        assert_eq!(signal.expect("no watch signal within 10s"), Some(false));

        // Dropping the handle releases the sender: the channel closes.
        drop(handle);
        let closed = tokio::time::timeout(Duration::from_secs(10), async {
            while rx.recv().await.is_some() {}
        })
        .await;
        assert!(closed.is_ok(), "channel did not close after drop");
    }
}
