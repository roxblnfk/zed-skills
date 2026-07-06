//! Domain types shared across pipeline stages.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::traits::Vendor;

/// Directory name of a skill. This is the unique key inside the sync target
/// and the key used for conflict detection.
///
/// Not to be confused with the *canonical name* (frontmatter `name:`), which
/// is what `skills: [...]` allowlists match against.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SkillId(String);

impl SkillId {
    pub fn new(dir_name: impl Into<String>) -> Self {
        Self(dir_name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SkillId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Name of a skill donor, e.g. `acme/skills` or `dir/skills-src`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VendorName(String);

impl VendorName {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for VendorName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifier of a vendor provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderId {
    Dir,
    Composer,
    Github,
    Gitlab,
    /// By-url remote entries (`from: http|zip`).
    Url,
}

impl ProviderId {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderId::Dir => "dir",
            ProviderId::Composer => "composer",
            ProviderId::Github => "github",
            ProviderId::Gitlab => "gitlab",
            ProviderId::Url => "url",
        }
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a vendor's content comes from. Serialized into the lockfile, so for
/// local vendors the path is stored exactly as declared in the manifest
/// (keeps the lockfile machine-independent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Origin {
    Local {
        path: String,
    },
    Remote {
        host: String,
        package: String,
        #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
        r#ref: Option<String>,
    },
    /// By-url donors (`from: http|zip`).
    Url {
        url: String,
    },
}

/// Tri-state `skills: [...]` allowlist from the manifest.
///
/// - `All` — absent/null entry: sync every skill of the donor.
/// - `Only(names)` — allowlist matched against *canonical* names.
///   `Only([])` means the donor is registered but pulls nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillsFilter {
    All,
    Only(Vec<String>),
}

impl SkillsFilter {
    pub fn from_manifest(value: Option<Vec<String>>) -> Self {
        match value {
            None => SkillsFilter::All,
            Some(names) => SkillsFilter::Only(names),
        }
    }

    pub fn allows(&self, canonical_name: &str) -> bool {
        match self {
            SkillsFilter::All => true,
            SkillsFilter::Only(names) => names.iter().any(|n| n == canonical_name),
        }
    }

    /// True for `Only([])` — donor registered, pulls nothing.
    pub fn is_empty_allowlist(&self) -> bool {
        matches!(self, SkillsFilter::Only(names) if names.is_empty())
    }
}

/// How a donor entered the run — determines which trust rules apply to it
/// (SPEC §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustBasis {
    /// Declared by the user in `skills.json` (`local.dir`, `remote[]`).
    /// Implicitly trusted — the user typed it.
    UserDeclared,
    /// Direct dependency of the root project (`require` / `require-dev`).
    /// Implicitly trusted unless `trusted-replace: true`.
    DirectDependency,
    /// Transitive local-provider discovery — must clear the effective trust
    /// list.
    Transitive,
}

/// Whether a donor declares where its skills live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DonorStatus {
    /// Declares its skills source (a manifest entry, or composer
    /// `extra.skills.source`).
    Declared,
    /// No declaration — a discovery candidate. Included only when discovery
    /// is enabled (globally or per-package via a positional argument).
    Undeclared,
    /// Declares a source that failed validation. Dropped from the run with
    /// a warning; never blocks other donors.
    Malformed { reason: String },
}

/// Locate-stage routing hint carried by a materialized vendor: tells the
/// locator chain which strategies apply (SPEC §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceHint {
    /// `local.dir` donors: the vendor root itself is the skills root.
    ExplicitRoot,
    /// Composer donors with a validated `extra.skills.source` (normalized
    /// `/`-separated path relative to the package root).
    Declared(String),
    /// Undeclared composer donors admitted via discovery: well-known
    /// containers, then the bounded recursive fallback.
    Discovery,
    /// Remote/url donors: probe composer.json, then well-known containers.
    Probe,
}

/// A discovered donor reference: output of the Discover stage, input of
/// Materialize. Carries the vendor handle used to materialize it.
#[derive(Clone)]
pub struct VendorRef {
    pub provider: ProviderId,
    pub name: VendorName,
    pub origin: Origin,
    pub filter: SkillsFilter,
    pub trust: TrustBasis,
    pub status: DonorStatus,
    pub vendor: Arc<dyn Vendor>,
}

impl fmt::Debug for VendorRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VendorRef")
            .field("provider", &self.provider)
            .field("name", &self.name)
            .field("origin", &self.origin)
            .field("filter", &self.filter)
            .field("trust", &self.trust)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

/// A vendor whose content is available on the local filesystem. After this
/// point local and remote vendors are indistinguishable.
#[derive(Debug, Clone)]
pub struct MaterializedVendor {
    pub name: VendorName,
    pub origin: Origin,
    /// Absolute directory holding the vendor's content.
    pub root: PathBuf,
    pub ref_resolved: Option<String>,
    pub filter: SkillsFilter,
    /// Which locator strategies apply to this vendor.
    pub source_hint: SourceHint,
}

/// A directory whose immediate subdirectories are candidate skills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillsRoot {
    pub path: PathBuf,
}

/// A skill found and scanned inside a materialized vendor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedSkill {
    /// Directory name (conflict-detection key).
    pub id: SkillId,
    /// Frontmatter `name:`, falling back to the directory name.
    pub canonical_name: String,
    pub description: Option<String>,
    pub vendor: VendorName,
    pub origin: Origin,
    pub ref_resolved: Option<String>,
    /// Absolute path of the skill directory inside the materialized vendor.
    pub path: PathBuf,
    /// Sorted relative file paths, `/`-separated.
    pub files: Vec<String>,
    /// SHA-256 over sorted relative paths + file contents.
    pub content_hash: String,
}

/// A skill that survived conflict detection and allowlist filtering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSkill {
    pub id: SkillId,
    pub canonical_name: String,
    pub description: Option<String>,
    pub vendor: VendorName,
    pub origin: Origin,
    pub ref_resolved: Option<String>,
    pub path: PathBuf,
    pub files: Vec<String>,
    pub content_hash: String,
}

impl From<ScannedSkill> for ResolvedSkill {
    fn from(s: ScannedSkill) -> Self {
        ResolvedSkill {
            id: s.id,
            canonical_name: s.canonical_name,
            description: s.description,
            vendor: s.vendor,
            origin: s.origin,
            ref_resolved: s.ref_resolved,
            path: s.path,
            files: s.files,
            content_hash: s.content_hash,
        }
    }
}

/// Diagnostic note surfaced at the end of a run (`[skip]` / `[hint]` blocks).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub kind: NoteKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteKind {
    Skip,
    Hint,
    Warn,
}

impl Note {
    pub fn skip(message: impl Into<String>) -> Self {
        Note {
            kind: NoteKind::Skip,
            message: message.into(),
        }
    }

    pub fn hint(message: impl Into<String>) -> Self {
        Note {
            kind: NoteKind::Hint,
            message: message.into(),
        }
    }

    pub fn warn(message: impl Into<String>) -> Self {
        Note {
            kind: NoteKind::Warn,
            message: message.into(),
        }
    }
}
