use std::path::Path;

use skills_core::manifest::MANIFEST_NAME;

use crate::CliError;

const STUB: &str = r#"{
    "$schema": "https://raw.githubusercontent.com/zed-skills/ai-skills/main/resources/skills.schema.json",
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
    std::fs::write(&path, STUB)
        .map_err(|e| CliError::config(format!("cannot write {MANIFEST_NAME}: {e}")))?;
    println!("Wrote {MANIFEST_NAME}");
    Ok(())
}
