//! `VendorPattern` — trust / filter patterns matching package names.
//!
//! Two shapes:
//! - `vendor/package` — exact name match
//! - `vendor/*`       — any package in the vendor namespace
//!
//! Bare `vendor` (no slash) and multi-slash strings are rejected: composer
//! package names contain exactly one slash, and we want users to be explicit
//! about whether they trust the whole vendor or one package.

use std::fmt;

/// A parsed trust/filter pattern. Keeps the original text for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorPattern {
    raw: String,
    vendor: String,
    /// `None` means wildcard (`vendor/*`).
    package: Option<String>,
}

impl VendorPattern {
    /// Parse a pattern; the error is a human-readable reason.
    pub fn parse(pattern: &str) -> Result<VendorPattern, String> {
        let invalid = || {
            format!(
                "invalid vendor pattern '{pattern}': expected 'vendor/package' or 'vendor/*' \
                 (exactly one '/')"
            )
        };
        let mut parts = pattern.split('/');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(vendor), Some(package), None) if !vendor.is_empty() && !package.is_empty() => {
                Ok(VendorPattern {
                    raw: pattern.to_string(),
                    vendor: vendor.to_string(),
                    package: (package != "*").then(|| package.to_string()),
                })
            }
            _ => Err(invalid()),
        }
    }

    /// Whether a full package name (`vendor/package`) matches this pattern.
    pub fn matches(&self, package_name: &str) -> bool {
        let Some((vendor, package)) = package_name.split_once('/') else {
            return false;
        };
        if vendor != self.vendor {
            return false;
        }
        match &self.package {
            None => true,
            Some(exact) => exact == package,
        }
    }

    /// Original textual pattern.
    pub fn as_str(&self) -> &str {
        &self.raw
    }
}

impl fmt::Display for VendorPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

/// Whether any pattern in the slice matches the package name.
pub fn matches_any(patterns: &[VendorPattern], package_name: &str) -> bool {
    patterns.iter().any(|p| p.matches(package_name))
}

/// A parsed npm trust pattern. Npm names are either a bare package (`lodash`)
/// or a scoped package (`@scope/name`), so the grammar has three shapes:
///
/// - `lodash` — exact bare package (no slash, non-empty, not `*`).
/// - `@scope/pkg` — exact scoped package.
/// - `@scope/*` — any package in a scope.
///
/// Rejected: bare `*`, empty/blank, multi-slash (`a/b/c`), `@scope` without a
/// `/`, `@/pkg`, and unscoped-with-slash (`pkg/sub`) — the last cannot be a
/// real npm name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpmPattern {
    raw: String,
    kind: NpmKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NpmKind {
    /// Exact bare package, e.g. `lodash`.
    Bare(String),
    /// Exact scoped package, e.g. `@scope/pkg`.
    Scoped { scope: String, package: String },
    /// Any package in a scope, e.g. `@scope/*` (holds the scope).
    ScopeWildcard(String),
}

impl NpmPattern {
    /// Parse an npm pattern; the error is a human-readable reason.
    pub fn parse(pattern: &str) -> Result<NpmPattern, String> {
        let invalid = || {
            format!(
                "invalid npm pattern '{pattern}': expected 'package', '@scope/package' or \
                 '@scope/*'"
            )
        };
        if pattern.trim().is_empty() {
            return Err(invalid());
        }
        if let Some(rest) = pattern.strip_prefix('@') {
            // Scoped: exactly one '/', both sides non-empty.
            let mut parts = rest.split('/');
            match (parts.next(), parts.next(), parts.next()) {
                (Some(scope), Some(package), None) if !scope.is_empty() && !package.is_empty() => {
                    let kind = if package == "*" {
                        NpmKind::ScopeWildcard(scope.to_string())
                    } else {
                        NpmKind::Scoped {
                            scope: scope.to_string(),
                            package: package.to_string(),
                        }
                    };
                    Ok(NpmPattern {
                        raw: pattern.to_string(),
                        kind,
                    })
                }
                _ => Err(invalid()),
            }
        } else {
            // Bare: no slash, and not the wildcard-only `*`.
            if pattern.contains('/') || pattern == "*" {
                return Err(invalid());
            }
            Ok(NpmPattern {
                raw: pattern.to_string(),
                kind: NpmKind::Bare(pattern.to_string()),
            })
        }
    }

    /// Whether an npm package name (`lodash` / `@scope/name`) matches.
    pub fn matches(&self, package_name: &str) -> bool {
        match &self.kind {
            NpmKind::Bare(pkg) => package_name == pkg,
            NpmKind::Scoped { scope, package } => package_name
                .strip_prefix('@')
                .and_then(|rest| rest.split_once('/'))
                .is_some_and(|(s, p)| s == scope && p == package),
            NpmKind::ScopeWildcard(scope) => package_name
                .strip_prefix('@')
                .and_then(|rest| rest.split_once('/'))
                .is_some_and(|(s, _)| s == scope),
        }
    }

    /// Original textual pattern.
    pub fn as_str(&self) -> &str {
        &self.raw
    }
}

impl fmt::Display for NpmPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

/// A manager-scoped trust pattern: composer names use the [`VendorPattern`]
/// grammar, npm names the [`NpmPattern`] grammar. Trust lists are per-manager,
/// so a donor is only ever matched against patterns of its own manager — a
/// composer pattern never approves an npm donor and vice versa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustPattern {
    Composer(VendorPattern),
    Npm(NpmPattern),
}

impl TrustPattern {
    /// Parse `pattern` under the grammar of `manager` (`composer` / `npm`).
    /// Returns `None` for managers without a trust grammar (e.g. `go`) or
    /// unknown ids.
    pub fn parse(manager: &str, pattern: &str) -> Option<Result<TrustPattern, String>> {
        match manager {
            "composer" => Some(VendorPattern::parse(pattern).map(TrustPattern::Composer)),
            "npm" => Some(NpmPattern::parse(pattern).map(TrustPattern::Npm)),
            _ => None,
        }
    }

    /// Whether a package name matches under this pattern's grammar.
    pub fn matches(&self, package_name: &str) -> bool {
        match self {
            TrustPattern::Composer(p) => p.matches(package_name),
            TrustPattern::Npm(p) => p.matches(package_name),
        }
    }

    /// Original textual pattern.
    pub fn as_str(&self) -> &str {
        match self {
            TrustPattern::Composer(p) => p.as_str(),
            TrustPattern::Npm(p) => p.as_str(),
        }
    }
}

impl fmt::Display for TrustPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether any trust pattern in the slice matches the package name.
pub fn trust_matches_any(patterns: &[TrustPattern], package_name: &str) -> bool {
    patterns.iter().any(|p| p.matches(package_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exact_and_wildcard() {
        let exact = VendorPattern::parse("acme/skills").unwrap();
        assert_eq!(exact.as_str(), "acme/skills");
        assert!(exact.matches("acme/skills"));
        assert!(!exact.matches("acme/other"));
        assert!(!exact.matches("other/skills"));

        let wild = VendorPattern::parse("acme/*").unwrap();
        assert!(wild.matches("acme/skills"));
        assert!(wild.matches("acme/anything"));
        assert!(!wild.matches("acmex/skills"));
    }

    #[test]
    fn rejects_bare_vendor_multi_slash_and_empty_sides() {
        for bad in ["acme", "a/b/c", "/pkg", "vendor/", "", "/", "*"] {
            let err = VendorPattern::parse(bad).unwrap_err();
            assert!(err.contains("invalid vendor pattern"), "{bad}: {err}");
        }
    }

    #[test]
    fn literal_star_package_is_wildcard_only_when_whole_segment() {
        // `acme/*x` is an exact package named `*x`, not a wildcard.
        let odd = VendorPattern::parse("acme/*x").unwrap();
        assert!(odd.matches("acme/*x"));
        assert!(!odd.matches("acme/anything"));
    }

    #[test]
    fn name_without_slash_never_matches() {
        let wild = VendorPattern::parse("acme/*").unwrap();
        assert!(!wild.matches("acme"));
    }

    #[test]
    fn matches_any_over_a_set() {
        let set = [
            VendorPattern::parse("acme/*").unwrap(),
            VendorPattern::parse("other/pkg").unwrap(),
        ];
        assert!(matches_any(&set, "acme/anything"));
        assert!(matches_any(&set, "other/pkg"));
        assert!(!matches_any(&set, "other/nope"));
        assert!(!matches_any(&[], "acme/anything"));
    }

    // --- npm grammar --------------------------------------------------------

    #[test]
    fn npm_bare_exact_match() {
        let p = NpmPattern::parse("lodash").unwrap();
        assert_eq!(p.as_str(), "lodash");
        assert!(p.matches("lodash"));
        assert!(!p.matches("lodash-es"));
        assert!(!p.matches("@scope/lodash"));
    }

    #[test]
    fn npm_scoped_exact_match() {
        let p = NpmPattern::parse("@scope/pkg").unwrap();
        assert!(p.matches("@scope/pkg"));
        assert!(!p.matches("@scope/other"));
        assert!(!p.matches("@other/pkg"));
        assert!(!p.matches("scope/pkg"));
        assert!(!p.matches("pkg"));
    }

    #[test]
    fn npm_scope_wildcard_match() {
        let p = NpmPattern::parse("@scope/*").unwrap();
        assert!(p.matches("@scope/pkg"));
        assert!(p.matches("@scope/anything"));
        assert!(!p.matches("@scopex/pkg"));
        assert!(!p.matches("@scope"));
        assert!(!p.matches("scope/pkg"));
        assert!(!p.matches("pkg"));
    }

    #[test]
    fn npm_wildcard_is_only_the_whole_package_segment() {
        // `@scope/*x` is an exact package named `*x`, not a wildcard.
        let p = NpmPattern::parse("@scope/*x").unwrap();
        assert!(p.matches("@scope/*x"));
        assert!(!p.matches("@scope/anything"));
    }

    #[test]
    fn npm_rejects_bad_shapes() {
        for bad in [
            "*",       // bare wildcard
            "",        // empty
            "   ",     // blank
            "a/b/c",   // multi-slash
            "@scope",  // scope without '/'
            "@",       // just '@'
            "@/pkg",   // empty scope
            "@scope/", // empty package
            "pkg/sub", // unscoped with slash
        ] {
            let err = NpmPattern::parse(bad).unwrap_err();
            assert!(err.contains("invalid npm pattern"), "{bad}: {err}");
        }
    }

    #[test]
    fn trust_pattern_dispatches_by_manager() {
        let composer = TrustPattern::parse("composer", "acme/*").unwrap().unwrap();
        assert!(matches!(composer, TrustPattern::Composer(_)));
        assert!(composer.matches("acme/skills"));

        let npm = TrustPattern::parse("npm", "@scope/*").unwrap().unwrap();
        assert!(matches!(npm, TrustPattern::Npm(_)));
        assert!(npm.matches("@scope/thing"));

        // Managers without a trust grammar / unknown ids.
        assert!(TrustPattern::parse("go", "github.com/a/*").is_none());
        assert!(TrustPattern::parse("cargo", "serde").is_none());

        // Bad patterns still surface a parse error (Some(Err)).
        assert!(TrustPattern::parse("npm", "*").unwrap().is_err());
    }

    #[test]
    fn trust_pattern_grammars_do_not_cross_approve() {
        // A composer pattern must not approve an npm scoped name, even though
        // both textually contain a slash.
        let composer = TrustPattern::parse("composer", "@scope/thing")
            .unwrap()
            .unwrap();
        // Composer parses `@scope/thing` as vendor `@scope`, package `thing`.
        assert!(composer.matches("@scope/thing"));
        // An npm scope-wildcard donor name should be matched by npm grammar.
        let npm = TrustPattern::parse("npm", "@scope/*").unwrap().unwrap();
        assert!(npm.matches("@scope/thing"));
        // Cross-grammar isolation is enforced by trust.rs matching a donor only
        // against its own manager's list; see the trust-stage tests.
    }

    #[test]
    fn trust_matches_any_over_a_set() {
        let set = [
            TrustPattern::parse("npm", "@scope/*").unwrap().unwrap(),
            TrustPattern::parse("npm", "lodash").unwrap().unwrap(),
        ];
        assert!(trust_matches_any(&set, "@scope/anything"));
        assert!(trust_matches_any(&set, "lodash"));
        assert!(!trust_matches_any(&set, "@other/pkg"));
        assert!(!trust_matches_any(&[], "lodash"));
    }
}
