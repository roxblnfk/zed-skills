//! `skills add <url|from:package|./path>` — register a donor in
//! `skills.json`'s `sources[]`, validate it actually ships skills, then sync.
//!
//! Two input shapes are accepted:
//!
//! - a **repo** (shorthand / web / ssh / scp URL) → a `github`/`gitlab`
//!   source. Flow (PHP `skills:add` parity): parse the input; resolve the
//!   concrete ref (explicit ref verbatim, caret via tag listing, absent ref
//!   via the cascade); decide the stored ref (user-typed wins verbatim, an
//!   auto-resolved stable tag is stored as `^X.Y.Z`, anything else stores no
//!   ref); materialize + scan the donor — refuse to register one that yields
//!   no skills; upsert into `skills.json` (dedupe by `from|host|package`);
//!   run a full `update`.
//!
//! - a **path** (`./x`, `../x`, `/abs`, `X:\...`) → a `dir` source (PHP
//!   `skills:add ./skills` parity). `--ref` is rejected (a local directory
//!   has no ref); the declared path is resolved against the cwd and must
//!   exist and ship at least one skill. The entry is `{ "from": "dir",
//!   "path": "<declared>" }` (deduped by `dir|default|<path>`).

use std::path::Path;
use std::sync::Arc;

use serde_json::{Map, Value, json};

use skills_core::domain::{ProviderId, SkillsFilter, VendorName};
use skills_core::manifest::{MANIFEST_NAME, Manifest, SCHEMA_URL};
use skills_core::paths::{join_declared, normalize_declared};
use skills_core::pipeline::ctx::CACHE_DIR;
use skills_core::pipeline::scan::scan_vendor;
use skills_core::traits::{Cache, Vendor};
use skills_providers::{
    DirVendor, GithubVendor, GitlabVendor, parse_add_input, refresolver, vendor_name_from_dir,
};

use crate::CliError;

pub async fn run(
    cwd: &Path,
    input: &str,
    r#ref: Option<String>,
    skills: Vec<String>,
    host: Option<String>,
) -> Result<(), CliError> {
    // 1. Parse. A path-shaped input selects the dir adapter.
    let parsed = parse_add_input(input, host.as_deref())
        .map_err(|e| CliError::config(format!("add: {e}")))?;

    if parsed.from == ProviderId::Dir {
        return add_dir(cwd, parsed.path.unwrap_or_default(), r#ref, skills).await;
    }
    add_repo(cwd, parsed.from, parsed.package, parsed.host, r#ref, skills).await
}

/// Register a local `dir` donor. The declared path (already
/// `normalize_declared`d by the parser) is resolved against the cwd, must
/// exist, and must ship at least one skill.
async fn add_dir(
    cwd: &Path,
    declared: String,
    r#ref: Option<String>,
    skills: Vec<String>,
) -> Result<(), CliError> {
    // A local directory has no ref — `--ref` never reaches the parser, so the
    // rejection lives here.
    if r#ref.is_some() {
        return Err(CliError::config(
            "add: '--ref' is not applicable to a dir source",
        ));
    }

    let root = join_declared(cwd, &declared);
    if !root.is_dir() {
        return Err(CliError::config(format!(
            "add: directory does not exist: '{declared}'"
        )));
    }
    // The vendor name derives from the DECLARED path; only machine-specific
    // outward shapes fall back to the canonical dir, so canonicalize for that
    // argument (a failure on an existing dir is a config error).
    let canonical = std::fs::canonicalize(&root).map_err(|e| {
        CliError::config(format!("add: cannot resolve directory '{declared}': {e}"))
    })?;
    let name = vendor_name_from_dir(&declared, &canonical).ok_or_else(|| {
        CliError::config(format!("add: dir path has no directory name: '{declared}'"))
    })?;

    // Validate: the directory must actually ship skills.
    let vendor: Arc<dyn Vendor> = Arc::new(DirVendor::new(
        VendorName::new(name),
        declared.clone(),
        root,
        SkillsFilter::All,
    ));
    let scanned = materialize_and_scan(cwd, &vendor).await.map_err(|e| {
        CliError::provider(format!(
            "add: dir:{declared} ships no recognizable skills root: {e}"
        ))
    })?;
    if scanned == 0 {
        return Err(CliError::provider(format!(
            "add: dir:{declared} ships no skills - refusing to register it as a donor"
        )));
    }

    // Upsert into skills.json (dedupe by dir|default|<path>).
    upsert_manifest_entry(cwd, "dir", "", None, None, Some(&declared), &skills)?;
    println!("Registered dir:{declared}");

    sync_after_add(cwd).await
}

/// Register a remote `github`/`gitlab` donor.
async fn add_repo(
    cwd: &Path,
    from: ProviderId,
    package: String,
    host: Option<String>,
    r#ref: Option<String>,
    skills: Vec<String>,
) -> Result<(), CliError> {
    // 2 + 3. Resolve the concrete ref and decide what to store.
    let http = super::http_client()?;
    let vendor: Arc<dyn Vendor>;
    let resolved: String;
    match from {
        ProviderId::Github => {
            let v = GithubVendor::new(
                Arc::clone(&http),
                package.clone(),
                host.clone(),
                r#ref.clone(),
                std::env::var("GITHUB_TOKEN").ok(),
            );
            resolved = v.resolve_ref().await.map_err(CliError::provider)?;
            vendor = Arc::new(v);
        }
        ProviderId::Gitlab => {
            let v = GitlabVendor::new(
                Arc::clone(&http),
                package.clone(),
                host.clone(),
                r#ref.clone(),
                std::env::var("GITLAB_TOKEN").ok(),
            );
            resolved = v.resolve_ref().await.map_err(CliError::provider)?;
            vendor = Arc::new(v);
        }
        other => {
            return Err(CliError::config(format!(
                "add: provider '{other}' is not supported"
            )));
        }
    }
    let stored_ref = match &r#ref {
        // Explicit user-typed ref (or caret constraint) is stored verbatim.
        Some(user_typed) => Some(user_typed.clone()),
        // Auto-cascade onto a stable tag stores its caret form; a
        // prerelease or default-branch resolution stores no ref.
        None => refresolver::format_caret(&resolved),
    };

    // 4. Fetch + validate: the repo must actually yield skills.
    let scanned = materialize_and_scan(cwd, &vendor).await.map_err(|e| {
        CliError::provider(format!(
            "add: {from}:{package} ships no recognizable skills root (declared \
             composer.json source or a well-known container): {e}"
        ))
    })?;
    if scanned == 0 {
        return Err(CliError::provider(format!(
            "add: {from}:{package} @ {resolved} ships no skills - refusing to register it as a donor"
        )));
    }

    // 5. Upsert into skills.json.
    upsert_manifest_entry(
        cwd,
        from.as_str(),
        &package,
        host.as_deref(),
        stored_ref.as_deref(),
        None,
        &skills,
    )?;
    println!(
        "Registered {from}:{package}{}",
        match &stored_ref {
            Some(r) => format!(" @ {r}"),
            None => String::new(),
        }
    );

    sync_after_add(cwd).await
}

/// Materialize + scan a candidate donor, returning the number of skills it
/// ships. Errors carry the raw scan failure for the caller to wrap.
async fn materialize_and_scan(cwd: &Path, vendor: &Arc<dyn Vendor>) -> Result<usize, String> {
    let cache = Cache::new(cwd.join(CACHE_DIR));
    let materialized = vendor
        .materialize(&cache)
        .await
        .map_err(|e| e.to_string())?;
    let scanned = scan_vendor(materialized, super::locators())
        .await
        .map_err(|e| e.to_string())?;
    Ok(scanned.len())
}

/// The trailing full `update` run shared by both add paths.
async fn sync_after_add(cwd: &Path) -> Result<(), CliError> {
    super::update::run(
        cwd,
        /* dry_run */ false,
        /* check */ false,
        None,
        Vec::new(),
        None,
        false,
        false,
        super::RawFilters {
            packages: Vec::new(),
            trust: Vec::new(),
        },
    )
    .await
}

/// Insert or update a `sources[]` entry, preserving the rest of the manifest.
/// The uniqueness key is `dir|default|<normalized path>` for `dir` entries and
/// `from|host|package` otherwise (mirroring the manifest validator). The file
/// is rewritten with 2-space indentation.
///
/// Auto-migration: a manifest that still uses the deprecated `remote` key
/// (and no `sources`) is upgraded in place — the key is renamed to
/// `sources`, preserving its entries and their order. A manifest that sets
/// both keys is rejected before any write.
fn upsert_manifest_entry(
    cwd: &Path,
    from: &str,
    package: &str,
    host: Option<&str>,
    stored_ref: Option<&str>,
    path: Option<&str>,
    skills: &[String],
) -> Result<(), CliError> {
    let manifest_path = cwd.join(MANIFEST_NAME);
    let mut doc: Value = if manifest_path.is_file() {
        let raw = std::fs::read_to_string(&manifest_path)
            .map_err(|e| CliError::config(format!("cannot read {MANIFEST_NAME}: {e}")))?;
        serde_json::from_str(&raw).map_err(|e| CliError::config(format!("{MANIFEST_NAME}: {e}")))?
    } else {
        // Fresh manifest: point editors at the published JSON Schema
        // (existing files are never retrofitted).
        json!({ "$schema": SCHEMA_URL })
    };
    let Value::Object(root) = &mut doc else {
        return Err(CliError::config(format!(
            "{MANIFEST_NAME}: top level must be a JSON object"
        )));
    };

    // Auto-migrate the deprecated alias: rename `remote` -> `sources`
    // (preserving its entries and order). Setting both keys is a config
    // error — refuse before touching anything.
    if root.contains_key("remote") {
        if root.contains_key("sources") {
            return Err(CliError::config(format!(
                "{MANIFEST_NAME} sets both 'sources' and its deprecated alias 'remote'; \
                 keep only 'sources'"
            )));
        }
        let entries = root.remove("remote").unwrap_or(Value::Array(Vec::new()));
        root.insert("sources".to_string(), entries);
    }

    let sources = root
        .entry("sources")
        .or_insert_with(|| Value::Array(Vec::new()));
    let Value::Array(entries) = sources else {
        return Err(CliError::config(format!(
            "{MANIFEST_NAME}: 'sources' must be an array"
        )));
    };

    // Uniqueness key mirrors `validate_source_entry`: `dir` entries key on the
    // normalized `path` (host is always `default`, no ref); everything else
    // keys on `from|host|package`.
    let key = |entry: &Map<String, Value>| -> String {
        let efrom = entry.get("from").and_then(Value::as_str).unwrap_or("");
        if efrom == "dir" {
            format!(
                "dir|default|{}",
                normalize_declared(entry.get("path").and_then(Value::as_str).unwrap_or(""))
            )
        } else {
            format!(
                "{efrom}|{}|{}",
                entry
                    .get("host")
                    .and_then(Value::as_str)
                    .unwrap_or("default"),
                entry.get("package").and_then(Value::as_str).unwrap_or(""),
            )
        }
    };
    let new_key = if from == "dir" {
        format!("dir|default|{}", normalize_declared(path.unwrap_or("")))
    } else {
        format!("{from}|{}|{package}", host.unwrap_or("default"))
    };

    // Merge policy on re-add: the fresh ref decision wins; an existing
    // allowlist survives (even an empty one — "pulls nothing" is a
    // deliberate state) and unions with any `--skill` flags.
    let mut merged_skills: Vec<String> = Vec::new();
    let mut had_allowlist = false;
    let mut replace_at: Option<usize> = None;
    for (idx, entry) in entries.iter().enumerate() {
        let Value::Object(map) = entry else { continue };
        if key(map) == new_key {
            replace_at = Some(idx);
            if let Some(Value::Array(existing)) = map.get("skills") {
                had_allowlist = true;
                merged_skills.extend(
                    existing
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string),
                );
            }
            break;
        }
    }
    for name in skills {
        if !merged_skills.contains(name) {
            merged_skills.push(name.clone());
        }
    }

    let mut entry = Map::new();
    entry.insert("from".to_string(), json!(from));
    if from == "dir" {
        entry.insert("path".to_string(), json!(path.unwrap_or("")));
    } else {
        entry.insert("package".to_string(), json!(package));
        if let Some(host) = host {
            entry.insert("host".to_string(), json!(host));
        }
        if let Some(r) = stored_ref {
            entry.insert("ref".to_string(), json!(r));
        }
    }
    // An absent allowlist means "all skills" and must stay absent unless
    // the user asked for one (or one already existed).
    if had_allowlist || !skills.is_empty() {
        entry.insert("skills".to_string(), json!(merged_skills));
    }

    match replace_at {
        Some(idx) => entries[idx] = Value::Object(entry),
        None => entries.push(Value::Object(entry)),
    }

    let mut rendered = serde_json::to_string_pretty(&doc)
        .map_err(|e| CliError::config(format!("cannot serialize {MANIFEST_NAME}: {e}")))?;
    rendered.push('\n');
    // Never write a manifest the tool itself would refuse to load.
    Manifest::parse(&rendered).map_err(|e| {
        CliError::config(format!("refusing to write an invalid {MANIFEST_NAME}: {e}"))
    })?;
    std::fs::write(&manifest_path, rendered)
        .map_err(|e| CliError::config(format!("cannot write {MANIFEST_NAME}: {e}")))?;
    Ok(())
}
