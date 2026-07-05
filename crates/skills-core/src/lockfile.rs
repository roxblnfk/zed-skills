//! `skills.lock` — committed lockfile driving the Plan diff, pruning and
//! the non-destructive merge guarantees.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::domain::{Origin, ResolvedSkill, SkillId};
use crate::error::LockfileError;
use crate::fsutil;

pub const LOCKFILE_NAME: &str = "skills.lock";
pub const LOCKFILE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lockfile {
    pub version: u32,
    pub skills: Vec<LockedSkill>,
}

impl Default for Lockfile {
    fn default() -> Self {
        Lockfile {
            version: LOCKFILE_VERSION,
            skills: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedSkill {
    pub id: String,
    pub vendor: String,
    pub origin: Origin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_resolved: Option<String>,
    pub content_hash: String,
    /// Sorted relative paths (`/`-separated). Sync owns exactly these files.
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit: Option<AuditCacheEntry>,
}

/// Cached audit verdict, keyed by content hash + auditor set (M4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditCacheEntry {
    pub verdict: String,
    pub auditor_set_hash: String,
}

impl From<&ResolvedSkill> for LockedSkill {
    fn from(skill: &ResolvedSkill) -> Self {
        LockedSkill {
            id: skill.id.as_str().to_string(),
            vendor: skill.vendor.as_str().to_string(),
            origin: skill.origin.clone(),
            ref_resolved: skill.ref_resolved.clone(),
            content_hash: skill.content_hash.clone(),
            files: skill.files.clone(),
            audit: None,
        }
    }
}

impl Lockfile {
    /// Load the lockfile; `Ok(None)` if the file does not exist.
    pub fn load(path: &Path) -> Result<Option<Lockfile>, LockfileError> {
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(path).map_err(|source| LockfileError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let lock: Lockfile = serde_json::from_str(&raw).map_err(LockfileError::Parse)?;
        Ok(Some(lock))
    }

    /// Deterministic serialization: skills sorted by id, pretty JSON with a
    /// trailing newline.
    pub fn to_json_string(&self) -> String {
        let mut sorted = self.clone();
        sorted.skills.sort_by(|a, b| a.id.cmp(&b.id));
        let mut s = serde_json::to_string_pretty(&sorted).expect("lockfile serializes");
        s.push('\n');
        s
    }

    pub fn save(&self, path: &Path) -> Result<(), LockfileError> {
        std::fs::write(path, self.to_json_string()).map_err(|source| LockfileError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn find(&self, id: &SkillId) -> Option<&LockedSkill> {
        self.skills.iter().find(|s| s.id == id.as_str())
    }
}

/// Sync status of one locked skill against the target directory, as shown by
/// `skills show`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStatus {
    /// All lock-listed files exist and match the locked content hash.
    Synced,
    /// A lock-listed file is missing or its content differs. User-added
    /// files are not considered.
    Modified,
    /// Not present in the lockfile.
    NotSynced,
}

/// Compute drift for a locked skill: re-hash the lock-listed files as found
/// in `skill_target_dir` and compare with the locked hash.
pub fn sync_status(skill_target_dir: &Path, locked: &LockedSkill) -> SyncStatus {
    match fsutil::content_hash(skill_target_dir, &locked.files) {
        Ok(hash) if hash == locked.content_hash => SyncStatus::Synced,
        // Missing files or unreadable content count as drift.
        _ => SyncStatus::Modified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Lockfile {
        Lockfile {
            version: LOCKFILE_VERSION,
            skills: vec![
                LockedSkill {
                    id: "zeta".into(),
                    vendor: "dir/src".into(),
                    origin: Origin::Local {
                        path: "./src".into(),
                    },
                    ref_resolved: None,
                    content_hash: "hash-z".into(),
                    files: vec!["SKILL.md".into()],
                    audit: None,
                },
                LockedSkill {
                    id: "alpha".into(),
                    vendor: "dir/src".into(),
                    origin: Origin::Local {
                        path: "./src".into(),
                    },
                    ref_resolved: Some("v1".into()),
                    content_hash: "hash-a".into(),
                    files: vec!["SKILL.md".into(), "refs/a.md".into()],
                    audit: None,
                },
            ],
        }
    }

    #[test]
    fn roundtrip_and_sorted_output() {
        let lock = sample();
        let json = lock.to_json_string();
        // Serialization sorts by id.
        let alpha = json.find("\"alpha\"").unwrap();
        let zeta = json.find("\"zeta\"").unwrap();
        assert!(alpha < zeta);
        let parsed: Lockfile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.skills.len(), 2);
        assert_eq!(parsed.version, LOCKFILE_VERSION);
    }

    #[test]
    fn save_and_load() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(LOCKFILE_NAME);
        sample().save(&path).unwrap();
        let loaded = Lockfile::load(&path).unwrap().unwrap();
        assert_eq!(loaded.skills.len(), 2);
        assert_eq!(
            loaded.find(&SkillId::new("alpha")).unwrap().ref_resolved,
            Some("v1".into())
        );
    }

    #[test]
    fn load_missing_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(
            Lockfile::load(&tmp.path().join(LOCKFILE_NAME))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn unknown_lockfile_key_rejected() {
        let raw = r#"{ "version": 1, "skills": [], "extra": true }"#;
        assert!(serde_json::from_str::<Lockfile>(raw).is_err());
    }

    #[test]
    fn origin_serialization_shape() {
        let json = sample().to_json_string();
        assert!(json.contains("\"type\": \"local\""), "{json}");
        assert!(json.contains("\"path\": \"./src\""), "{json}");
    }

    #[test]
    fn sync_status_detects_drift() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("SKILL.md"), "content").unwrap();
        let files = vec!["SKILL.md".to_string()];
        let hash = fsutil::content_hash(tmp.path(), &files).unwrap();
        let locked = LockedSkill {
            id: "s".into(),
            vendor: "dir/src".into(),
            origin: Origin::Local { path: "./x".into() },
            ref_resolved: None,
            content_hash: hash,
            files,
            audit: None,
        };
        assert_eq!(sync_status(tmp.path(), &locked), SyncStatus::Synced);

        // User-added file is not drift.
        std::fs::write(tmp.path().join("user-notes.md"), "mine").unwrap();
        assert_eq!(sync_status(tmp.path(), &locked), SyncStatus::Synced);

        // Modified lock-listed file is drift.
        std::fs::write(tmp.path().join("SKILL.md"), "edited").unwrap();
        assert_eq!(sync_status(tmp.path(), &locked), SyncStatus::Modified);

        // Missing lock-listed file is drift.
        std::fs::remove_file(tmp.path().join("SKILL.md")).unwrap();
        assert_eq!(sync_status(tmp.path(), &locked), SyncStatus::Modified);
    }
}
