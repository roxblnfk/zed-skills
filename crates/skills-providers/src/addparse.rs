//! Input parsing for `skills add <url|from:package>`.
//!
//! Accepted grammars:
//!
//! - `github:owner/repo` / `gitlab:group[/sub...]/project` shorthand;
//! - web URLs (`https://github.com/owner/repo`,
//!   `https://gitlab.com/a/b/c.git?x#y` — path captured greedily,
//!   `.git` / query / fragment stripped);
//! - SSH URLs (`ssh://git@host[:port]/path.git`);
//! - SCP-style clone URLs (`git@host:path.git` — the path must contain a
//!   `/`, otherwise `host:thing` is not a clone URL).
//!
//! Provider inference for URLs is exact-host only (`github.com` /
//! `gitlab.com`); anything else needs the `from:` shorthand plus `--host`.

use skills_core::domain::ProviderId;
use skills_core::paths::{is_absolute_like, normalize_declared};

use crate::remote::normalize_host;
use crate::{github, gitlab};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAdd {
    /// `ProviderId::Github` / `ProviderId::Gitlab` for repo inputs;
    /// `ProviderId::Dir` for a path-shaped input.
    pub from: ProviderId,
    /// The repo `owner/repo[/...]` identifier. Empty for a dir input.
    pub package: String,
    /// `None` = the provider's public default host. Always `None` for dir.
    pub host: Option<String>,
    /// The declared donor path for a dir input (`normalize_declared`d,
    /// `/`-separated, kept as typed — relative stays relative, absolute stays
    /// absolute). `None` for repo inputs.
    pub path: Option<String>,
}

/// Parse the `skills add` positional input, reconciling a `--host`
/// override. Errors are human-readable one-liners.
pub fn parse_add_input(input: &str, host_override: Option<&str>) -> Result<ParsedAdd, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("input must not be empty".to_string());
    }

    // Path-shaped input selects the dir adapter — checked BEFORE the
    // URL/shorthand rules so `./x`, `../x`, `/abs`, `\abs`, `X:\...` never
    // fall through to repo parsing. `owner/repo` and URLs carry none of these
    // prefixes and route unchanged.
    if looks_like_path(input) {
        // A host is meaningless for a local directory. (`--ref` never reaches
        // this parser — the CLI passes it separately — so the CLI rejects a
        // ref on a dir input.)
        if host_override.is_some() {
            return Err("'--host' is not applicable to a dir source".to_string());
        }
        return Ok(ParsedAdd {
            from: ProviderId::Dir,
            package: String::new(),
            host: None,
            path: Some(normalize_declared(input)),
        });
    }

    // Shorthand: from:package.
    if let Some(package) = input.strip_prefix("github:") {
        return finish(
            ProviderId::Github,
            package,
            host_override.map(str::to_string),
        );
    }
    if let Some(package) = input.strip_prefix("gitlab:") {
        return finish(
            ProviderId::Gitlab,
            package,
            host_override.map(str::to_string),
        );
    }

    // Web URL: scheme://host/path.
    if let Some((host, path)) = split_web_url(input) {
        let from = infer_provider(&host)?;
        let host = reconcile_host(host_override, &host)?;
        return finish(from, &path, host);
    }

    // SSH URL: ssh://[user@]host[:port]/path.
    if let Some((host, path)) = split_ssh_url(input) {
        let from = infer_provider(&host)?;
        let host = reconcile_host(host_override, &host)?;
        return finish(from, &path, host);
    }

    // SCP form: [user@]host:path (path must contain '/').
    if let Some((host, path)) = split_scp(input) {
        let from = infer_provider(&host)?;
        let host = reconcile_host(host_override, &host)?;
        return finish(from, &path, host);
    }

    Err(format!(
        "cannot parse '{input}': expected github:owner/repo, gitlab:group/project \
         or a repository URL (https/ssh/scp)"
    ))
}

fn finish(from: ProviderId, package: &str, host: Option<String>) -> Result<ParsedAdd, String> {
    let package = package.trim_matches('/').to_string();
    match from {
        ProviderId::Github => github::validate_package(&package)?,
        ProviderId::Gitlab => gitlab::validate_package(&package)?,
        _ => unreachable!("add only dispatches to github/gitlab"),
    }
    Ok(ParsedAdd {
        from,
        package,
        host,
        path: None,
    })
}

/// Whether an input is path-shaped: an explicit `./` / `../` prefix (either
/// separator) or an absolute path (`/x`, `\x`, `C:\x`, `C:/x`). `owner/repo`
/// and `host:port`-style clone URLs carry none of these prefixes.
fn looks_like_path(input: &str) -> bool {
    input.starts_with("./")
        || input.starts_with("../")
        || input.starts_with(".\\")
        || input.starts_with("..\\")
        || is_absolute_like(input)
}

/// `https?://host/path[.git][?...][#...]` → `(scheme://host, path)`.
fn split_web_url(input: &str) -> Option<(String, String)> {
    let rest = input
        .strip_prefix("https://")
        .map(|r| ("https://", r))
        .or_else(|| input.strip_prefix("http://").map(|r| ("http://", r)));
    let (scheme, rest) = rest?;
    let (host, path) = rest.split_once('/')?;
    if host.is_empty() {
        return None;
    }
    Some((format!("{scheme}{host}"), strip_path_suffixes(path)))
}

/// `ssh://[user@]host[:port]/path[.git]` → `(https://host, path)`.
/// The port belongs to SSH, not to the HTTPS API — it is dropped.
fn split_ssh_url(input: &str) -> Option<(String, String)> {
    let rest = input.strip_prefix("ssh://")?;
    let (authority, path) = rest.split_once('/')?;
    let host_port = match authority.rsplit_once('@') {
        Some((_user, hp)) => hp,
        None => authority,
    };
    let host = match host_port.split_once(':') {
        Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => host,
        Some(_) => return None,
        None => host_port,
    };
    if host.is_empty() {
        return None;
    }
    Some((format!("https://{host}"), strip_path_suffixes(path)))
}

/// SCP-style `[user@]host:path[.git]` → `(https://host, path)`. Requires a
/// `/` in the path so `host:1234` is not mistaken for a clone URL.
fn split_scp(input: &str) -> Option<(String, String)> {
    if input.contains("://") || input.contains(char::is_whitespace) {
        return None;
    }
    let (authority, path) = input.split_once(':')?;
    if !path.contains('/') {
        return None;
    }
    let host = match authority.rsplit_once('@') {
        Some((user, host)) if !user.is_empty() => host,
        Some(_) => return None,
        None => authority,
    };
    if host.is_empty() || host.contains('/') {
        return None;
    }
    Some((format!("https://{host}"), strip_path_suffixes(path)))
}

/// Drop query, fragment, a trailing `.git` and stray slashes.
fn strip_path_suffixes(path: &str) -> String {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let path = path.trim_matches('/');
    path.strip_suffix(".git").unwrap_or(path).to_string()
}

/// Exact-host provider inference for URL inputs.
fn infer_provider(host: &str) -> Result<ProviderId, String> {
    let bare = match host.find("://") {
        Some(idx) => &host[idx + 3..],
        None => host,
    }
    .to_ascii_lowercase();
    match bare.as_str() {
        "github.com" => Ok(ProviderId::Github),
        "gitlab.com" => Ok(ProviderId::Gitlab),
        _ => Err(format!(
            "cannot infer the provider from host '{bare}'; use the shorthand form with \
             an explicit host, e.g. `skills add gitlab:group/project --host={bare}`"
        )),
    }
}

/// Reconcile a `--host` override with the URL-derived host; default hosts
/// stay implicit (`None`) so the stored config remains terse.
fn reconcile_host(host_override: Option<&str>, url_host: &str) -> Result<Option<String>, String> {
    if let Some(over) = host_override
        && normalize_host(over) != normalize_host(url_host)
    {
        return Err(format!(
            "--host={over} conflicts with the URL host {url_host}"
        ));
    }
    let normalized = normalize_host(url_host);
    if normalized == "https://github.com" || normalized == "https://gitlab.com" {
        return Ok(None);
    }
    Ok(Some(host_override.unwrap_or(url_host).to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(from: ProviderId, package: &str, host: Option<&str>) -> ParsedAdd {
        ParsedAdd {
            from,
            package: package.to_string(),
            host: host.map(str::to_string),
            path: None,
        }
    }

    fn dir(path: &str) -> ParsedAdd {
        ParsedAdd {
            from: ProviderId::Dir,
            package: String::new(),
            host: None,
            path: Some(path.to_string()),
        }
    }

    #[test]
    fn path_shaped_inputs_select_dir() {
        assert_eq!(parse_add_input("./skills", None).unwrap(), dir("skills"));
        assert_eq!(
            parse_add_input("../shared", None).unwrap(),
            dir("../shared")
        );
        assert_eq!(
            parse_add_input("/abs/skills", None).unwrap(),
            dir("/abs/skills")
        );
        assert_eq!(
            parse_add_input(r"C:\shared\skills", None).unwrap(),
            dir("C:/shared/skills")
        );
        assert_eq!(
            parse_add_input(r".\win\style", None).unwrap(),
            dir("win/style")
        );
    }

    #[test]
    fn repo_inputs_never_route_to_dir() {
        // Shorthand and URLs must be unaffected by the path branch. A plain
        // `owner/repo` (no path prefix) still needs the `from:` shorthand.
        assert!(parse_add_input("owner/repo", None).is_err());
        assert_eq!(
            parse_add_input("github:owner/repo", None).unwrap(),
            parsed(ProviderId::Github, "owner/repo", None)
        );
        assert_eq!(
            parse_add_input("https://github.com/acme/skills", None).unwrap(),
            parsed(ProviderId::Github, "acme/skills", None)
        );
    }

    #[test]
    fn host_with_path_is_rejected() {
        let err = parse_add_input("./skills", Some("github.com")).unwrap_err();
        assert!(err.contains("not applicable to a dir source"), "{err}");
    }

    #[test]
    fn shorthand_forms() {
        assert_eq!(
            parse_add_input("github:acme/skills", None).unwrap(),
            parsed(ProviderId::Github, "acme/skills", None)
        );
        assert_eq!(
            parse_add_input("gitlab:org/group/sub/project", None).unwrap(),
            parsed(ProviderId::Gitlab, "org/group/sub/project", None)
        );
        // Shorthand + --host stays as given.
        assert_eq!(
            parse_add_input("gitlab:a/b", Some("gitlab.example.com")).unwrap(),
            parsed(ProviderId::Gitlab, "a/b", Some("gitlab.example.com"))
        );
    }

    #[test]
    fn shorthand_segment_validation() {
        assert!(parse_add_input("github:onlyowner", None).is_err());
        assert!(parse_add_input("github:a/b/c", None).is_err());
        assert!(parse_add_input("gitlab:project", None).is_err());
        assert!(parse_add_input("gitlab:a//b", None).is_err());
        assert!(parse_add_input("github:", None).is_err());
    }

    #[test]
    fn web_urls() {
        assert_eq!(
            parse_add_input("https://github.com/acme/skills", None).unwrap(),
            parsed(ProviderId::Github, "acme/skills", None)
        );
        assert_eq!(
            parse_add_input("https://github.com/acme/skills.git", None).unwrap(),
            parsed(ProviderId::Github, "acme/skills", None)
        );
        // Greedy multi-segment GitLab path, query/fragment stripped.
        assert_eq!(
            parse_add_input(
                "https://gitlab.com/org/group/sub/project.git?ref=x#frag",
                None
            )
            .unwrap(),
            parsed(ProviderId::Gitlab, "org/group/sub/project", None)
        );
        assert_eq!(
            parse_add_input("https://gitlab.com/a/b/", None).unwrap(),
            parsed(ProviderId::Gitlab, "a/b", None)
        );
    }

    #[test]
    fn unknown_url_host_needs_shorthand() {
        let err = parse_add_input("https://ghe.corp.example/acme/skills", None).unwrap_err();
        assert!(err.contains("cannot infer"), "{err}");
        assert!(err.contains("--host"), "{err}");
    }

    #[test]
    fn ssh_urls() {
        assert_eq!(
            parse_add_input("ssh://git@gitlab.com/org/sub/project.git", None).unwrap(),
            parsed(ProviderId::Gitlab, "org/sub/project", None)
        );
        // Port is dropped; trailing slash tolerated.
        assert_eq!(
            parse_add_input("ssh://git@gitlab.com:2222/a/b/", None).unwrap(),
            parsed(ProviderId::Gitlab, "a/b", None)
        );
        assert_eq!(
            parse_add_input("ssh://github.com/acme/skills.git", None).unwrap(),
            parsed(ProviderId::Github, "acme/skills", None)
        );
    }

    #[test]
    fn scp_forms() {
        assert_eq!(
            parse_add_input("git@github.com:acme/skills.git", None).unwrap(),
            parsed(ProviderId::Github, "acme/skills", None)
        );
        assert_eq!(
            parse_add_input("git@gitlab.com:org/group/project.git", None).unwrap(),
            parsed(ProviderId::Gitlab, "org/group/project", None)
        );
        // No '/' in the path: not a clone URL.
        assert!(parse_add_input("gitlab.com:8080", None).is_err());
    }

    #[test]
    fn host_conflicts_are_rejected() {
        let err = parse_add_input("https://gitlab.com/a/b", Some("https://gitlab.example.com"))
            .unwrap_err();
        assert!(err.contains("conflicts"), "{err}");
        // Matching override is fine (and default host stays implicit).
        assert_eq!(
            parse_add_input("https://gitlab.com/a/b", Some("gitlab.com")).unwrap(),
            parsed(ProviderId::Gitlab, "a/b", None)
        );
    }

    #[test]
    fn garbage_is_rejected_with_a_hint() {
        let err = parse_add_input("not a repo", None).unwrap_err();
        assert!(err.contains("github:owner/repo"), "{err}");
        assert!(parse_add_input("", None).is_err());
    }
}
