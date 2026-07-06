//! Pure ref-resolution rules for remote providers (SPEC §7, ported from the
//! PHP `RefResolver` — a deliberately narrow semver subset).
//!
//! - **Stable tag**: `v?X.Y.Z`, exactly three numeric components, no suffix.
//! - **Any-semver**: stable plus an optional `-<prerelease>` suffix
//!   (`[A-Za-z0-9_.+-]+`).
//! - **Cascade** (ref absent): highest stable → highest any-semver
//!   (stable > prerelease; prerelease ties break lexically) → default branch.
//! - **Caret** `^A[.B[.C]]`: floor `[A, B|0, C|0]`, ceiling `[A+1, 0, 0]`,
//!   highest *stable* tag in range. `^0.x` is unsupported (pre-1.0 caret
//!   semantics differ) and never matches.
//! - Anything else is a verbatim ref (tag / branch / SHA).

/// `(major, minor, patch)` of a stable tag; the optional `v` prefix is
/// ignored for comparison but preserved in returned tag strings.
fn parse_stable(tag: &str) -> Option<(u64, u64, u64)> {
    let (triplet, prerelease) = parse_any(tag)?;
    prerelease.is_empty().then_some(triplet)
}

/// `((major, minor, patch), prerelease)` of any semver-shaped tag;
/// `prerelease` is empty for stable tags.
fn parse_any(tag: &str) -> Option<((u64, u64, u64), &str)> {
    let rest = tag.strip_prefix('v').unwrap_or(tag);
    let (core, prerelease) = match rest.split_once('-') {
        Some((core, suffix)) => {
            if suffix.is_empty()
                || !suffix
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '+' | '-'))
            {
                return None;
            }
            (core, suffix)
        }
        None => (rest, ""),
    };
    let mut parts = core.split('.');
    let major = parse_component(parts.next()?)?;
    let minor = parse_component(parts.next()?)?;
    let patch = parse_component(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some(((major, minor, patch), prerelease))
}

fn parse_component(s: &str) -> Option<u64> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok())?
}

/// Whether `tag` is a stable semver tag (three components, no suffix).
pub fn is_stable(tag: &str) -> bool {
    parse_stable(tag).is_some()
}

/// Whether `tag` is semver-shaped at all (stable OR prerelease).
pub fn is_semver(tag: &str) -> bool {
    parse_any(tag).is_some()
}

/// Whether `r#ref` is a caret constraint (`^1`, `^1.2`, `^v1.2.3`).
pub fn is_caret(r#ref: &str) -> bool {
    parse_caret(r#ref).is_some()
}

fn parse_caret(constraint: &str) -> Option<(u64, u64, u64)> {
    let rest = constraint.strip_prefix('^')?;
    let rest = rest.strip_prefix('v').unwrap_or(rest);
    let mut parts = rest.split('.');
    let major = parse_component(parts.next()?)?;
    let minor = match parts.next() {
        Some(s) => parse_component(s)?,
        None => 0,
    };
    let patch = match parts.next() {
        Some(s) => parse_component(s)?,
        None => 0,
    };
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Highest stable tag in the list (semver order on the triplet).
pub fn pick_highest_stable(tags: &[String]) -> Option<&str> {
    tags.iter()
        .filter_map(|tag| parse_stable(tag).map(|parts| (parts, tag)))
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, tag)| tag.as_str())
}

/// Highest semver-shaped tag overall. Stable outranks prerelease on an
/// equal triplet; among prereleases the suffix breaks ties lexically.
pub fn pick_highest_any(tags: &[String]) -> Option<&str> {
    tags.iter()
        .filter_map(|tag| parse_any(tag).map(|(parts, pre)| ((parts, pre.is_empty(), pre), tag)))
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, tag)| tag.as_str())
}

/// Resolve a caret constraint against a tag list: highest stable tag in
/// `[floor, ceiling)`, or `None` when nothing matches. `^0.x` never
/// matches (unsupported pre-1.0 semantics).
pub fn resolve_caret<'t>(constraint: &str, tags: &'t [String]) -> Option<&'t str> {
    let (major, minor, patch) = parse_caret(constraint)?;
    if major < 1 {
        return None;
    }
    let floor = (major, minor, patch);
    let ceiling = (major + 1, 0, 0);
    tags.iter()
        .filter_map(|tag| parse_stable(tag).map(|parts| (parts, tag)))
        .filter(|(parts, _)| *parts >= floor && *parts < ceiling)
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, tag)| tag.as_str())
}

/// Format a stable tag as the `^X.Y.Z` constraint stored by `skills add`
/// (leading `v` stripped). `None` when the input is not a stable tag —
/// the caller stores no ref and the cascade re-runs on every sync.
pub fn format_caret(stable_tag: &str) -> Option<String> {
    let (major, minor, patch) = parse_stable(stable_tag)?;
    Some(format!("^{major}.{minor}.{patch}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn stable_tag_classification() {
        let stable = ["1.2.3", "v1.2.3", "0.0.1", "10.20.30", "v0.0.0"];
        for tag in stable {
            assert!(is_stable(tag), "expected stable: {tag}");
            assert!(is_semver(tag), "stable implies semver: {tag}");
        }
        let not_stable = [
            "1.2.3-rc.1",
            "v1",
            "1.2",
            "v1.2",
            "1.2.3.4",
            "1.2.x",
            "1.02.a",
            "release-1.2.3",
            "1.2.3 ",
            " 1.2.3",
            "vv1.2.3",
            "",
            "main",
        ];
        for tag in not_stable {
            assert!(!is_stable(tag), "expected not stable: {tag:?}");
        }
    }

    #[test]
    fn any_semver_classification() {
        let semver = [
            "1.2.3",
            "v1.2.3-rc.1",
            "1.2.3-beta",
            "1.2.3-alpha.2+build-7",
            "1.2.3-a_b",
        ];
        for tag in semver {
            assert!(is_semver(tag), "expected semver: {tag}");
        }
        let not_semver = ["1.2-rc.1", "1.2.3-", "1.2.3-rc 1", "v1.2.3-rc/1", "main"];
        for tag in not_semver {
            assert!(!is_semver(tag), "expected not semver: {tag:?}");
        }
    }

    #[test]
    fn caret_classification() {
        for c in ["^1", "^1.2", "^1.2.3", "^v1.2.3", "^0.2.3"] {
            assert!(is_caret(c), "expected caret: {c}");
        }
        for c in ["1.2.3", "^1.2.3.4", "^", "^v", "^1.x", "~1.2", ">=1.0"] {
            assert!(!is_caret(c), "expected not caret: {c:?}");
        }
    }

    #[test]
    fn highest_stable_picks_semver_order_not_lexical() {
        let t = tags(&["1.9.0", "1.10.0", "1.2.0"]);
        assert_eq!(pick_highest_stable(&t), Some("1.10.0"));
    }

    #[test]
    fn highest_stable_ignores_prereleases_and_junk() {
        let t = tags(&["2.0.0-rc.1", "main", "1.5.2", "v1.6.0", "nightly"]);
        assert_eq!(pick_highest_stable(&t), Some("v1.6.0"));
        assert_eq!(pick_highest_stable(&tags(&["main", "dev"])), None);
        assert_eq!(pick_highest_stable(&[]), None);
    }

    #[test]
    fn highest_stable_preserves_v_prefix_verbatim() {
        let t = tags(&["v3.0.0", "2.9.9"]);
        assert_eq!(pick_highest_stable(&t), Some("v3.0.0"));
    }

    #[test]
    fn highest_any_prefers_stable_over_prerelease_on_same_triplet() {
        let t = tags(&["1.0.0-rc.2", "1.0.0"]);
        assert_eq!(pick_highest_any(&t), Some("1.0.0"));
    }

    #[test]
    fn highest_any_falls_back_to_prereleases() {
        let t = tags(&["2.0.0-alpha", "2.0.0-beta", "main"]);
        assert_eq!(pick_highest_any(&t), Some("2.0.0-beta"));
        // Higher triplet beats stability of a lower one.
        let t = tags(&["1.0.0", "2.0.0-rc.1"]);
        assert_eq!(pick_highest_any(&t), Some("2.0.0-rc.1"));
    }

    #[test]
    fn prerelease_tiebreak_is_lexical() {
        let t = tags(&["1.0.0-rc.10", "1.0.0-rc.9"]);
        // Lexical, not numeric: "rc.9" > "rc.10".
        assert_eq!(pick_highest_any(&t), Some("1.0.0-rc.9"));
    }

    #[test]
    fn highest_any_none_when_nothing_semver_shaped() {
        assert_eq!(pick_highest_any(&tags(&["main", "release"])), None);
    }

    #[test]
    fn caret_full_floor_and_major_ceiling() {
        let t = tags(&["1.2.2", "1.2.3", "1.9.0", "2.0.0", "2.1.0"]);
        assert_eq!(resolve_caret("^1.2.3", &t), Some("1.9.0"));
        assert_eq!(resolve_caret("^1", &t), Some("1.9.0"));
        assert_eq!(resolve_caret("^2", &t), Some("2.1.0"));
    }

    #[test]
    fn caret_short_forms_default_missing_components_to_zero() {
        let t = tags(&["1.0.0", "1.4.9", "1.5.0"]);
        assert_eq!(resolve_caret("^1.5", &t), Some("1.5.0"));
        assert_eq!(resolve_caret("^1.6", &t), None);
    }

    #[test]
    fn caret_floor_is_inclusive_ceiling_exclusive() {
        let t = tags(&["1.2.3", "2.0.0"]);
        assert_eq!(resolve_caret("^1.2.3", &t), Some("1.2.3"));
        assert_eq!(resolve_caret("^2.0.0", &t), Some("2.0.0"));
        let t = tags(&["2.0.0"]);
        assert_eq!(resolve_caret("^1.0.0", &t), None);
    }

    #[test]
    fn caret_ignores_prereleases_entirely() {
        let t = tags(&["1.5.0-rc.1", "1.4.0"]);
        assert_eq!(resolve_caret("^1.0.0", &t), Some("1.4.0"));
        let t = tags(&["1.5.0-rc.1"]);
        assert_eq!(resolve_caret("^1.0.0", &t), None);
    }

    #[test]
    fn caret_v_prefixes_work_both_sides() {
        let t = tags(&["v1.3.0"]);
        assert_eq!(resolve_caret("^v1.2.3", &t), Some("v1.3.0"));
        assert_eq!(resolve_caret("^1.2", &t), Some("v1.3.0"));
    }

    #[test]
    fn caret_pre_1_0_is_unsupported() {
        let t = tags(&["0.2.3", "0.2.9", "0.3.0"]);
        assert_eq!(resolve_caret("^0.2.3", &t), None);
        assert_eq!(resolve_caret("^0", &t), None);
    }

    #[test]
    fn format_caret_strips_v_and_requires_stable() {
        assert_eq!(format_caret("v1.2.3").as_deref(), Some("^1.2.3"));
        assert_eq!(format_caret("1.2.3").as_deref(), Some("^1.2.3"));
        assert_eq!(format_caret("1.2.3-rc.1"), None);
        assert_eq!(format_caret("main"), None);
        assert_eq!(format_caret("v1.2"), None);
    }
}
