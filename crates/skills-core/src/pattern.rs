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
}
