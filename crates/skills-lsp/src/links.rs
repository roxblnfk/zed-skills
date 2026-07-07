//! `textDocument/documentLink` for `skills.json`: make manifest values
//! clickable.
//!
//! Links are anchored to the *value* spans (the whole string literal,
//! quotes included) via the existing [`SpanIndex`]:
//!
//! - by-package `remote[]` entries (`from: github|gitlab`) — the `package`
//!   value links to the repo web URL. Host semantics mirror the providers'
//!   M2 API-base handling ([`skills_providers::normalize_host`]): a bare
//!   host gets `https://`, a declared scheme is kept, trailing slashes are
//!   dropped; absent host = `github.com` / `gitlab.com`. Other `from`
//!   values (by-url handled below, unknown sources) get no package link.
//! - by-url entries (`from: http|zip`) — the `url` value links verbatim.
//! - `local.dir` entries — the dir string links to the `file://` URI of the
//!   resolved directory, only when it exists on disk.
//!
//! A malformed/unparseable manifest yields an empty list (no errors); no
//! `resolve` support — every link ships its target eagerly.

use std::path::Path;
use std::str::FromStr;

use tower_lsp_server::ls_types::{DocumentLink, Uri};

use skills_core::manifest::{Manifest, PathSeg};
use skills_providers::normalize_host;

use crate::spanindex::SpanIndex;

/// Tooltip on by-package repo links.
pub const TOOLTIP_REPO: &str = "Open repository";
/// Tooltip on by-url links.
pub const TOOLTIP_URL: &str = "Open URL";
/// Tooltip on `local.dir` links.
pub const TOOLTIP_DIR: &str = "Open directory";

/// All document links of a `skills.json` buffer. `project_root` resolves
/// relative `local.dir` entries.
pub fn document_links(project_root: &Path, text: &str) -> Vec<DocumentLink> {
    let Ok(manifest) = serde_json::from_str::<Manifest>(text) else {
        return Vec::new();
    };
    let span = SpanIndex::new(text);
    let mut links = Vec::new();

    // local.dir → file:// URI of the resolved existing directory.
    for (idx, dir) in manifest.local_dirs().iter().enumerate() {
        let Ok(norm) = skills_core::paths::normalize_rel(dir) else {
            continue; // empty/absolute/escaping — validation flags it
        };
        let abs = project_root.join(skills_core::paths::rel_to_path(&norm));
        if !abs.is_dir() {
            continue;
        }
        let path = [
            PathSeg::key("local"),
            PathSeg::key("dir"),
            PathSeg::Index(idx),
        ];
        push_link(
            &mut links,
            &span,
            &path,
            Uri::from_file_path(&abs),
            TOOLTIP_DIR,
        );
    }

    // remote[] → repo web URL (by-package) / the URL verbatim (by-url).
    for (idx, entry) in manifest.remote.iter().flatten().enumerate() {
        let entry_path = |field: &str| {
            [
                PathSeg::key("remote"),
                PathSeg::Index(idx),
                PathSeg::key(field),
            ]
        };
        match entry.from.as_str() {
            "github" | "gitlab" => {
                let Some(url) = entry
                    .package
                    .as_deref()
                    .and_then(|package| repo_web_url(&entry.from, package, entry.host.as_deref()))
                else {
                    continue;
                };
                let target = Uri::from_str(&url).ok();
                push_link(
                    &mut links,
                    &span,
                    &entry_path("package"),
                    target,
                    TOOLTIP_REPO,
                );
            }
            "http" | "zip" => {
                let target = entry.url.as_deref().and_then(|url| Uri::from_str(url).ok());
                push_link(&mut links, &span, &entry_path("url"), target, TOOLTIP_URL);
            }
            _ => {} // unknown source — no link
        }
    }

    links
}

fn push_link(
    links: &mut Vec<DocumentLink>,
    span: &SpanIndex<'_>,
    path: &[PathSeg],
    target: Option<Uri>,
    tooltip: &str,
) {
    let Some(target) = target else { return };
    let Some(range) = span.range_of(path) else {
        return; // buffer/manifest disagree mid-edit — skip, don't guess
    };
    links.push(DocumentLink {
        range,
        target: Some(target),
        tooltip: Some(tooltip.to_string()),
        data: None,
    });
}

/// Repo web URL for a by-package entry, `None` for unknown sources or a
/// blank package. Absent/blank host = the provider's public host.
pub fn repo_web_url(from: &str, package: &str, host: Option<&str>) -> Option<String> {
    let package = package.trim().trim_matches('/');
    if package.is_empty() {
        return None;
    }
    let default_host = match from {
        "github" => skills_providers::github::GITHUB_DEFAULT_HOST,
        "gitlab" => skills_providers::gitlab::GITLAB_DEFAULT_HOST,
        _ => return None,
    };
    let base = match host.map(str::trim).filter(|h| !h.is_empty()) {
        Some(host) => normalize_host(host),
        None => format!("https://{default_host}"),
    };
    Some(format!("{base}/{package}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_default_host() {
        assert_eq!(
            repo_web_url("github", "acme/skills", None).unwrap(),
            "https://github.com/acme/skills"
        );
    }

    #[test]
    fn gitlab_default_host_and_nested_subgroups() {
        assert_eq!(
            repo_web_url("gitlab", "org/group/sub/project", None).unwrap(),
            "https://gitlab.com/org/group/sub/project"
        );
    }

    #[test]
    fn custom_host_without_scheme_gets_https() {
        assert_eq!(
            repo_web_url("gitlab", "grp/proj", Some("gitlab.example.com")).unwrap(),
            "https://gitlab.example.com/grp/proj"
        );
        assert_eq!(
            repo_web_url("github", "a/b", Some("ghe.corp.example")).unwrap(),
            "https://ghe.corp.example/a/b"
        );
    }

    #[test]
    fn custom_host_with_scheme_kept_trailing_slash_dropped() {
        assert_eq!(
            repo_web_url("gitlab", "grp/proj", Some("http://127.0.0.1:8080/")).unwrap(),
            "http://127.0.0.1:8080/grp/proj"
        );
        assert_eq!(
            repo_web_url("github", "a/b", Some("https://ghe.corp/")).unwrap(),
            "https://ghe.corp/a/b"
        );
    }

    #[test]
    fn blank_host_falls_back_to_default() {
        assert_eq!(
            repo_web_url("github", "a/b", Some("  ")).unwrap(),
            "https://github.com/a/b"
        );
    }

    #[test]
    fn unknown_from_or_blank_package_yields_none() {
        assert_eq!(repo_web_url("svn", "a/b", None), None);
        assert_eq!(repo_web_url("http", "a/b", None), None);
        assert_eq!(repo_web_url("github", "  ", None), None);
    }

    fn summary(links: &[DocumentLink]) -> Vec<(String, String, String)> {
        links
            .iter()
            .map(|l| {
                (
                    format!(
                        "{}:{}-{}:{}",
                        l.range.start.line,
                        l.range.start.character,
                        l.range.end.line,
                        l.range.end.character
                    ),
                    l.target
                        .as_ref()
                        .map(|t| t.as_str().to_string())
                        .unwrap_or_default(),
                    l.tooltip.clone().unwrap_or_default(),
                )
            })
            .collect()
    }

    #[test]
    fn links_anchor_value_spans_for_all_entry_kinds() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("skills-src")).unwrap();
        let text = r#"{
  "local": { "dir": ["./skills-src", "./missing"] },
  "remote": [
    { "from": "github", "package": "acme/skills" },
    { "from": "gitlab", "package": "org/group/sub/project", "host": "gitlab.example.com" },
    { "from": "zip", "url": "https://example.com/skills.zip" }
  ]
}"#;
        let links = document_links(tmp.path(), text);
        let got = summary(&links);
        assert_eq!(got.len(), 4, "{got:?}");

        // local.dir[0] exists → file URI; local.dir[1] missing → skipped.
        assert_eq!(got[0].0, "1:21-1:35"); // "./skills-src" incl. quotes
        assert!(got[0].1.starts_with("file://"), "{}", got[0].1);
        assert!(got[0].1.ends_with("/skills-src"), "{}", got[0].1);
        assert_eq!(got[0].2, TOOLTIP_DIR);

        assert_eq!(
            got[1],
            (
                "3:35-3:48".to_string(), // "acme/skills"
                "https://github.com/acme/skills".to_string(),
                TOOLTIP_REPO.to_string()
            )
        );
        assert_eq!(
            got[2],
            (
                "4:35-4:58".to_string(), // "org/group/sub/project"
                "https://gitlab.example.com/org/group/sub/project".to_string(),
                TOOLTIP_REPO.to_string()
            )
        );
        assert_eq!(
            got[3],
            (
                "5:28-5:60".to_string(), // the zip URL, verbatim
                "https://example.com/skills.zip".to_string(),
                TOOLTIP_URL.to_string()
            )
        );
    }

    #[test]
    fn unknown_from_and_missing_fields_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        // Semantically invalid entries (validation flags them) still must
        // not produce bogus links: unknown source, package-less github.
        let text = r#"{
  "remote": [
    { "from": "svn", "url": "https://svn.example.com/repo" },
    { "from": "github" }
  ]
}"#;
        assert!(document_links(tmp.path(), text).is_empty());
    }

    #[test]
    fn malformed_manifest_yields_empty_list() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(document_links(tmp.path(), "{ not json at all").is_empty());
        assert!(document_links(tmp.path(), r#"{ "remote": "nope" }"#).is_empty());
        assert!(document_links(tmp.path(), "").is_empty());
    }
}
