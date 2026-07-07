use std::path::Path;

use skills_core::manifest::{MANIFEST_NAME, SCHEMA_URL};
use skills_core::pipeline::ctx::CACHE_DIR;

use crate::CliError;

/// Stub manifest; `__SCHEMA_URL__` is replaced with
/// [`skills_core::manifest::SCHEMA_URL`] (editors with JSON Schema support
/// pick completion/validation up from the inline `$schema`).
const STUB: &str = r#"{
    "$schema": "__SCHEMA_URL__",
    "target": ".agents/skills",
    "local": {
        "dir": []
    }
}
"#;

pub fn run(cwd: &Path, force: bool) -> Result<(), CliError> {
    let path = cwd.join(MANIFEST_NAME);
    if path.exists() && !force {
        return Err(CliError::config(format!(
            "{MANIFEST_NAME} already exists (use --force to overwrite)"
        )));
    }
    std::fs::write(&path, STUB.replace("__SCHEMA_URL__", SCHEMA_URL))
        .map_err(|e| CliError::config(format!("cannot write {MANIFEST_NAME}: {e}")))?;
    println!("Wrote {MANIFEST_NAME}");
    ensure_cache_gitignored(cwd)?;
    Ok(())
}

/// Make sure the archive cache dir is gitignored: create `.gitignore` with
/// the entry, or append it to an existing one that lacks it.
fn ensure_cache_gitignored(cwd: &Path) -> Result<(), CliError> {
    let entry = format!("{CACHE_DIR}/");
    let path = cwd.join(".gitignore");
    let write_err = |e: std::io::Error| CliError::config(format!("cannot write .gitignore: {e}"));

    if !path.exists() {
        std::fs::write(&path, format!("{entry}\n")).map_err(write_err)?;
        println!("Wrote .gitignore ({entry})");
        return Ok(());
    }
    let existing = std::fs::read_to_string(&path)
        .map_err(|e| CliError::config(format!("cannot read .gitignore: {e}")))?;
    let already_ignored = existing
        .lines()
        .map(str::trim)
        .any(|line| line == entry || line == CACHE_DIR);
    if already_ignored {
        return Ok(());
    }
    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(&entry);
    updated.push('\n');
    std::fs::write(&path, updated).map_err(write_err)?;
    println!("Updated .gitignore ({entry})");
    Ok(())
}
