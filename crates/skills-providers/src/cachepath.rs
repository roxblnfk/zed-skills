//! Deterministic cache paths for fetched remote archives.
//!
//! Layout under the project-local cache root (`<project>/.skills-cache/`):
//!
//! ```text
//! <root>/<from>/<host-seg>/<id-seg>/<ref-seg>/     by-package entries
//! <root>/url/<sha256(url)[..16]>/<ref-seg>/        by-url entries
//! ```
//!
//! A cache entry is a *hit* only when the directory exists AND carries the
//! marker file — a directory without marker is a stale partial and gets
//! rebuilt. There is no TTL; invalidation is `skills update --refresh`.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Marker written after an archive is fully extracted into its cache dir.
pub const CACHE_MARKER: &str = ".skills-cache-ok";

/// Length of the URL-hash segment for by-url entries.
const URL_HASH_LEN: usize = 16;

/// 40-hex refs (full commit SHAs) are shortened to keep paths inside
/// Windows' default 260-char limit.
const SHA_PREFIX_LEN: usize = 12;

/// URL-safe segment encoding: `/` → `__`, `@` → `at-`, anything outside
/// `[A-Za-z0-9._-]` → `-`; an empty result becomes the literal `segment`.
pub fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for c in segment.chars() {
        match c {
            '/' => out.push_str("__"),
            '@' => out.push_str("at-"),
            c if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') => out.push(c),
            _ => out.push('-'),
        }
    }
    if out.is_empty() {
        "segment".to_string()
    } else {
        out
    }
}

/// Host segment: absent host → `default`; else scheme and trailing slashes
/// stripped, then segment-encoded. Ports are preserved (distinct endpoints
/// get distinct caches).
pub fn host_segment(host: Option<&str>) -> String {
    let Some(host) = host else {
        return "default".to_string();
    };
    let stripped = match host.find("://") {
        Some(idx) => &host[idx + 3..],
        None => host,
    };
    let stripped = stripped.trim_end_matches('/');
    if stripped.is_empty() {
        return "default".to_string();
    }
    encode_segment(stripped)
}

/// Ref segment: a full 40-hex SHA is truncated to its first 12 chars,
/// anything else is segment-encoded.
pub fn ref_segment(r#ref: &str) -> String {
    if r#ref.len() == 40 && r#ref.chars().all(|c| c.is_ascii_hexdigit()) {
        return r#ref[..SHA_PREFIX_LEN].to_ascii_lowercase();
    }
    encode_segment(r#ref)
}

/// First 16 hex chars of SHA-256 over the URL.
pub fn url_hash(url: &str) -> String {
    let digest = Sha256::digest(url.as_bytes());
    let mut out = String::with_capacity(URL_HASH_LEN);
    for byte in digest.iter() {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
        if out.len() >= URL_HASH_LEN {
            break;
        }
    }
    out.truncate(URL_HASH_LEN);
    out
}

/// Cache dir for a by-package entry: `<root>/<from>/<host>/<id>/<ref>/`.
pub fn entry_dir(
    cache_root: &Path,
    from: &str,
    host: Option<&str>,
    package: &str,
    resolved_ref: &str,
) -> PathBuf {
    cache_root
        .join(encode_segment(from))
        .join(host_segment(host))
        .join(encode_segment(package))
        .join(ref_segment(resolved_ref))
}

/// Cache dir for a by-url entry: `<root>/url/<sha256(url)[..16]>/<ref>/`.
pub fn url_dir(cache_root: &Path, url: &str, ref_label: &str) -> PathBuf {
    cache_root
        .join("url")
        .join(url_hash(url))
        .join(ref_segment(ref_label))
}

/// Cache hit = directory exists + marker file present.
pub fn is_hit(dir: &Path) -> bool {
    dir.is_dir() && dir.join(CACHE_MARKER).is_file()
}

/// Write the marker declaring `dir` a fully materialized cache entry. The
/// content (resolved ref / source URL) is informational only.
pub fn write_marker(dir: &Path, note: &str) -> std::io::Result<()> {
    std::fs::write(dir.join(CACHE_MARKER), note)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_segment_table() {
        // (input, expected)
        let cases = [
            ("acme/skills", "acme__skills"),
            ("org/group/sub/project", "org__group__sub__project"),
            ("@scope/pkg", "at-scope__pkg"),
            ("v1.2.3", "v1.2.3"),
            ("feature branch!", "feature-branch-"),
            ("a b:c", "a-b-c"),
            ("under_score-dash.dot", "under_score-dash.dot"),
            ("", "segment"),
            ("π", "-"),
        ];
        for (input, expected) in cases {
            assert_eq!(encode_segment(input), expected, "input: {input:?}");
        }
    }

    #[test]
    fn host_segment_table() {
        let cases = [
            (None, "default"),
            (Some(""), "default"),
            (Some("https://"), "default"),
            (Some("gitlab.example.com"), "gitlab.example.com"),
            (Some("https://gitlab.example.com"), "gitlab.example.com"),
            (Some("https://gitlab.example.com/"), "gitlab.example.com"),
            (Some("http://127.0.0.1:8080"), "127.0.0.1-8080"),
            (Some("HTTPS://Api.GitHub.com"), "Api.GitHub.com"),
        ];
        for (input, expected) in cases {
            assert_eq!(host_segment(input), expected, "input: {input:?}");
        }
    }

    #[test]
    fn ref_segment_table() {
        let full_sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(ref_segment(full_sha), "0123456789ab");
        // Uppercase SHA is still a SHA; truncation lowercases.
        let upper = "0123456789ABCDEF0123456789ABCDEF01234567";
        assert_eq!(ref_segment(upper), "0123456789ab");
        // 39 or 41 hex chars are not SHAs.
        assert_eq!(ref_segment(&full_sha[..39]), full_sha[..39].to_string());
        // Non-hex 40-char string is encoded, not truncated.
        let not_hex = "z123456789abcdef0123456789abcdef01234567";
        assert_eq!(ref_segment(not_hex), not_hex);
        assert_eq!(ref_segment("v1.2.3"), "v1.2.3");
        assert_eq!(ref_segment("feature/x"), "feature__x");
        assert_eq!(ref_segment(""), "segment");
    }

    #[test]
    fn url_hash_is_16_hex_and_deterministic() {
        let h = url_hash("https://example.com/skills.zip");
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(h, url_hash("https://example.com/skills.zip"));
        assert_ne!(h, url_hash("https://example.com/other.zip"));
    }

    #[test]
    fn entry_dir_layout() {
        let root = Path::new("cache");
        let dir = entry_dir(
            root,
            "gitlab",
            Some("gitlab.example.com"),
            "a/b/c",
            "v1.0.0",
        );
        let expected: PathBuf = ["cache", "gitlab", "gitlab.example.com", "a__b__c", "v1.0.0"]
            .iter()
            .collect();
        assert_eq!(dir, expected);

        let dir = entry_dir(root, "github", None, "acme/skills", "main");
        let expected: PathBuf = ["cache", "github", "default", "acme__skills", "main"]
            .iter()
            .collect();
        assert_eq!(dir, expected);
    }

    #[test]
    fn url_dir_layout() {
        let root = Path::new("cache");
        let dir = url_dir(root, "https://example.com/skills.zip", "latest");
        let expected: PathBuf = [
            "cache",
            "url",
            &url_hash("https://example.com/skills.zip"),
            "latest",
        ]
        .iter()
        .collect();
        assert_eq!(dir, expected);
    }

    #[test]
    fn hit_requires_dir_and_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("entry");
        assert!(!is_hit(&dir));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!is_hit(&dir), "dir without marker is a stale partial");
        write_marker(&dir, "v1.0.0").unwrap();
        assert!(is_hit(&dir));
    }
}
