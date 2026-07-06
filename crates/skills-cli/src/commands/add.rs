//! `skills add <url|from:package>` — register a remote donor in
//! `skills.json`, validate it actually ships skills, then sync.
//!
//! Flow (PHP `skills:add` parity):
//! 1. parse the input (shorthand / web / ssh / scp);
//! 2. resolve the concrete ref (explicit ref verbatim, caret via tag
//!    listing, absent ref via the cascade);
//! 3. decide the stored ref: user-typed wins verbatim; an auto-resolved
//!    stable tag is stored as `^X.Y.Z`; anything else stores no ref;
//! 4. materialize + scan the donor — refuse to register one that yields
//!    no skills;
//! 5. upsert the entry into `skills.json` (dedupe by `from|host|id`);
//! 6. run a full `update`.

use std::path::Path;
use std::sync::Arc;

use serde_json::{Map, Value, json};

use skills_core::domain::ProviderId;
use skills_core::manifest::{MANIFEST_NAME, Manifest};
use skills_core::pipeline::ctx::CACHE_DIR;
use skills_core::pipeline::scan::scan_vendor;
use skills_core::traits::{Cache, Vendor};
use skills_providers::{GithubVendor, GitlabVendor, parse_add_input, refresolver};

use crate::CliError;

pub async fn run(
    cwd: &Path,
    input: &str,
    r#ref: Option<String>,
    skills: Vec<String>,
    host: Option<String>,
) -> Result<(), CliError> {
    // 1. Parse.
    let parsed = parse_add_input(input, host.as_deref())
        .map_err(|e| CliError::config(format!("add: {e}")))?;

    // 2 + 3. Resolve the concrete ref and decide what to store.
    let http = super::http_client()?;
    let vendor: Arc<dyn Vendor>;
    let resolved: String;
    match parsed.from {
        ProviderId::Github => {
            let v = GithubVendor::new(
                Arc::clone(&http),
                parsed.package.clone(),
                parsed.host.clone(),
                r#ref.clone(),
                std::env::var("GITHUB_TOKEN").ok(),
            );
            resolved = v.resolve_ref().await.map_err(CliError::provider)?;
            vendor = Arc::new(v);
        }
        ProviderId::Gitlab => {
            let v = GitlabVendor::new(
                Arc::clone(&http),
                parsed.package.clone(),
                parsed.host.clone(),
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
    let cache = Cache::new(cwd.join(CACHE_DIR));
    let materialized = vendor
        .materialize(&cache)
        .await
        .map_err(|e| CliError::provider(e.to_string()))?;
    let scanned = scan_vendor(materialized, super::locators(false))
        .await
        .map_err(|e| {
            CliError::provider(format!(
                "add: {}:{} ships no recognizable skills root (declared \
                 composer.json source or a well-known container): {e}",
                parsed.from, parsed.package
            ))
        })?;
    if scanned.is_empty() {
        return Err(CliError::provider(format!(
            "add: {}:{} @ {resolved} ships no skills - refusing to register it as a donor",
            parsed.from, parsed.package
        )));
    }

    // 5. Upsert into skills.json.
    upsert_manifest_entry(
        cwd,
        parsed.from.as_str(),
        &parsed.package,
        parsed.host.as_deref(),
        stored_ref.as_deref(),
        &skills,
    )?;
    println!(
        "Registered {}:{}{}",
        parsed.from,
        parsed.package,
        match &stored_ref {
            Some(r) => format!(" @ {r}"),
            None => String::new(),
        }
    );

    // 6. Sync.
    super::update::run(
        cwd,
        false,
        None,
        None,
        false,
        false,
        super::RawFilters {
            packages: Vec::new(),
            trust: Vec::new(),
            discovery: false,
        },
    )
    .await
}

/// Insert or update the `remote[]` entry (uniqueness key:
/// `from|host|package`), preserving the rest of the manifest. The file is
/// rewritten with 2-space indentation.
fn upsert_manifest_entry(
    cwd: &Path,
    from: &str,
    package: &str,
    host: Option<&str>,
    stored_ref: Option<&str>,
    skills: &[String],
) -> Result<(), CliError> {
    let path = cwd.join(MANIFEST_NAME);
    let mut doc: Value = if path.is_file() {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| CliError::config(format!("cannot read {MANIFEST_NAME}: {e}")))?;
        serde_json::from_str(&raw).map_err(|e| CliError::config(format!("{MANIFEST_NAME}: {e}")))?
    } else {
        json!({})
    };
    let Value::Object(root) = &mut doc else {
        return Err(CliError::config(format!(
            "{MANIFEST_NAME}: top level must be a JSON object"
        )));
    };

    let remote = root
        .entry("remote")
        .or_insert_with(|| Value::Array(Vec::new()));
    let Value::Array(entries) = remote else {
        return Err(CliError::config(format!(
            "{MANIFEST_NAME}: 'remote' must be an array"
        )));
    };

    let key = |entry: &Map<String, Value>| -> String {
        format!(
            "{}|{}|{}",
            entry.get("from").and_then(Value::as_str).unwrap_or(""),
            entry
                .get("host")
                .and_then(Value::as_str)
                .unwrap_or("default"),
            entry.get("package").and_then(Value::as_str).unwrap_or(""),
        )
    };
    let new_key = format!("{from}|{}|{package}", host.unwrap_or("default"));

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
    entry.insert("package".to_string(), json!(package));
    if let Some(host) = host {
        entry.insert("host".to_string(), json!(host));
    }
    if let Some(r) = stored_ref {
        entry.insert("ref".to_string(), json!(r));
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
    std::fs::write(&path, rendered)
        .map_err(|e| CliError::config(format!("cannot write {MANIFEST_NAME}: {e}")))?;
    Ok(())
}
