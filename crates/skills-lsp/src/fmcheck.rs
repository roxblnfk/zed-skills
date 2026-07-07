//! SKILL.md frontmatter validation: duplicate keys, spec length limits,
//! `name` format, bool/enum values. Complements the textcheck rules in
//! skills-audit (missing frontmatter/description, name-mismatch, size) —
//! those stay there; everything here is keyed off the
//! [`FRONTMATTER_FIELDS`] table shared with completion, so the vocabulary,
//! the value kinds and the limits have a single source of truth.
//!
//! Canonical limits per the Agent Skills spec
//! (<https://agentskills.io/specification>, verified 2026-07-07): `name`
//! 1–64 chars, lowercase `a-z`/`0-9`/hyphens, no leading/trailing hyphen, no
//! consecutive hyphens; `description` 1–1024 chars; `compatibility` 1–500
//! chars. All findings are warnings — the sync-side reader never rejects a
//! skill over its frontmatter. The name-format rules themselves live in
//! [`skills_audit::name_format_error`] — the single implementation shared
//! with the StaticAuditor's and the LSP's directory-name checks.
//!
//! Pure functions over the document text (no filesystem, no LSP types), so
//! a future CLI StaticAuditor hookup is trivial. Frontmatter block/line
//! detection is [`skills_core::frontmatter::flat_entries`] — the exact rules
//! of the sync-side reader, which keeps the **last** non-absent value of a
//! duplicated key (pinned by a skills-core test).
//!
//! Unknown keys are legal (the spec is open) — no diagnostics for them.

use std::collections::HashMap;

use skills_audit::{TextCheck, name_format_error};
use skills_core::audit::Severity;
use skills_core::frontmatter::{FlatEntry, flat_entries};

use crate::completion::{FRONTMATTER_FIELDS, FrontmatterField, ValueKind};

/// Stable diagnostic codes for frontmatter validation.
pub mod codes {
    /// Same key set more than once inside the frontmatter block.
    pub const DUPLICATE: &str = "fm-duplicate";
    /// Value exceeds the Agent Skills spec length limit.
    pub const LENGTH: &str = "fm-length";
    /// `name` violates the spec name format.
    pub const FORMAT: &str = "fm-format";
    /// Bad bool/enum value, or an empty value where one is expected.
    pub const VALUE: &str = "fm-value";
}

/// Validate the frontmatter block of a SKILL.md document. Empty result when
/// the document has no (complete) frontmatter — the `no-frontmatter`
/// textcheck covers that case.
pub fn frontmatter_checks(text: &str) -> Vec<TextCheck> {
    let entries = flat_entries(text.as_bytes());
    let mut checks = Vec::new();
    let mut first_seen: HashMap<&str, usize> = HashMap::new();

    for entry in &entries {
        match first_seen.get(entry.key.as_str()) {
            Some(&first_line) => checks.push(warn(
                codes::DUPLICATE,
                format!(
                    "duplicate frontmatter key `{}` (first set on line {}; \
                     the last value wins during skills sync)",
                    entry.key,
                    first_line + 1, // human-facing: 1-based
                ),
                entry.line,
            )),
            None => {
                first_seen.insert(entry.key.as_str(), entry.line);
            }
        }
        if let Some(field) = FRONTMATTER_FIELDS.iter().find(|f| f.key == entry.key) {
            field_checks(field, entry, &mut checks);
        }
    }
    checks
}

/// Length / format / value checks for one known-field entry.
fn field_checks(field: &FrontmatterField, entry: &FlatEntry, checks: &mut Vec<TextCheck>) {
    if entry.is_absent() {
        // Empty `name:` silently falls back to the directory name — worth a
        // nudge. An empty `description:` is NOT flagged here: the reader
        // treats it as missing and the existing `no-description` textcheck
        // already fires (one diagnostic per problem).
        if field.key == "name" {
            checks.push(warn(
                codes::VALUE,
                "frontmatter `name` has no value — skills sync falls back to the \
                 directory name"
                    .to_string(),
                entry.line,
            ));
        }
        return;
    }

    let value = entry.value.as_str();
    if let Some(max) = field.max_len {
        let len = value.chars().count();
        if len > max {
            checks.push(warn(
                codes::LENGTH,
                format!(
                    "frontmatter `{}` is {len} characters (max {max} per the \
                     Agent Skills spec)",
                    field.key
                ),
                entry.line,
            ));
        }
    }

    match field.value {
        ValueKind::SkillName => {
            if let Some(reason) = name_format_error(value) {
                checks.push(warn(
                    codes::FORMAT,
                    format!("frontmatter `name` '{value}' {reason}"),
                    entry.line,
                ));
            }
        }
        ValueKind::Bool => {
            if !matches!(value, "true" | "false") {
                checks.push(warn(
                    codes::VALUE,
                    format!(
                        "frontmatter `{}` must be true or false (got '{value}')",
                        field.key
                    ),
                    entry.line,
                ));
            }
        }
        ValueKind::Enum(allowed) => {
            if !allowed.contains(&value) {
                checks.push(warn(
                    codes::VALUE,
                    format!(
                        "frontmatter `{}` must be one of {} (got '{value}')",
                        field.key,
                        allowed.join(", ")
                    ),
                    entry.line,
                ));
            }
        }
        ValueKind::Text | ValueKind::List | ValueKind::Map => {}
    }
}

fn warn(code: &'static str, message: String, line: usize) -> TextCheck {
    TextCheck {
        code,
        severity: Severity::Warn,
        message,
        line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checks(text: &str) -> Vec<TextCheck> {
        frontmatter_checks(text)
    }

    fn codes_lines(text: &str) -> Vec<(&'static str, usize)> {
        checks(text).iter().map(|c| (c.code, c.line)).collect()
    }

    #[test]
    fn clean_frontmatter_has_no_checks() {
        let text = "---\nname: deploy\ndescription: Deploys things.\n\
                    compatibility: Requires git\ndisable-model-invocation: true\n\
                    effort: high\ncontext: fork\nshell: powershell\n---\nbody\n";
        assert!(checks(text).is_empty());
    }

    #[test]
    fn no_or_unclosed_frontmatter_yields_nothing() {
        assert!(checks("# just a title\nname: X\n").is_empty());
        assert!(checks("---\nname: X\n").is_empty()); // unclosed, mid-typing
    }

    #[test]
    fn duplicate_key_warns_on_later_occurrences() {
        let text = "---\nname: a\ndescription: d\nname: b\n---\n";
        let found = checks(text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].code, codes::DUPLICATE);
        assert_eq!(found[0].line, 3);
        assert_eq!(found[0].severity, Severity::Warn);
        // Message references the FIRST line, 1-based, and the reader's
        // last-value-wins semantics.
        assert_eq!(
            found[0].message,
            "duplicate frontmatter key `name` (first set on line 2; \
             the last value wins during skills sync)"
        );
    }

    #[test]
    fn triple_duplicate_warns_twice_referencing_first() {
        let text = "---\nlicense: a\nlicense: b\nname: n\nlicense: c\n---\n";
        let found = checks(text);
        assert_eq!(
            found.iter().map(|c| (c.code, c.line)).collect::<Vec<_>>(),
            [(codes::DUPLICATE, 2), (codes::DUPLICATE, 4)]
        );
        for c in &found {
            assert!(c.message.contains("first set on line 2"), "{}", c.message);
        }
    }

    #[test]
    fn duplicate_unknown_key_still_flagged() {
        // The duplicate rule is not limited to known fields.
        let text = "---\nname: n\ncustom: a\ncustom: b\n---\n";
        assert_eq!(codes_lines(text), [(codes::DUPLICATE, 3)]);
    }

    #[test]
    fn indented_and_malformed_lines_do_not_count_as_keys() {
        // Mirrors the sync reader: nested lines and invalid keys are not
        // entries, so they can neither duplicate nor be duplicated.
        let text = "---\nmetadata:\n  name: nested\nname: n\nsome key: v\n---\n";
        assert!(checks(text).is_empty());
    }

    #[test]
    fn length_limits_at_boundary() {
        // (field, limit) triples straight from the table.
        for (key, limit) in [("name", 64), ("description", 1024), ("compatibility", 500)] {
            // Use a hyphen-free lowercase filler so `name` stays format-clean.
            let at = format!("---\n{key}: {}\n---\n", "a".repeat(limit));
            assert!(
                !checks(&at).iter().any(|c| c.code == codes::LENGTH),
                "{key} at limit {limit} must be clean"
            );
            let over = format!("---\n{key}: {}\n---\n", "a".repeat(limit + 1));
            let found = checks(&over);
            let hit = found
                .iter()
                .find(|c| c.code == codes::LENGTH)
                .unwrap_or_else(|| panic!("{key} over limit must warn"));
            assert_eq!(hit.line, 1);
            assert_eq!(
                hit.message,
                format!(
                    "frontmatter `{key}` is {} characters (max {limit} per the \
                     Agent Skills spec)",
                    limit + 1
                )
            );
        }
    }

    #[test]
    fn length_counts_characters_not_bytes() {
        // 64 two-byte characters: 128 bytes but exactly at the char limit.
        // (Also format-invalid — non-ASCII — but not over-length.)
        let text = format!("---\nname: {}\n---\n", "é".repeat(64));
        assert!(!checks(&text).iter().any(|c| c.code == codes::LENGTH));
    }

    #[test]
    fn unknown_fields_have_no_length_or_value_checks() {
        let long = "x".repeat(2000);
        let text = format!("---\nwhatever: {long}\ncustom: Not A Bool\n---\n");
        assert!(checks(&text).is_empty());
    }

    #[test]
    fn name_format_matrix() {
        for good in ["a", "z9", "pdf-processing", "a-b-c", "42"] {
            let text = format!("---\nname: {good}\n---\n");
            assert!(
                !checks(&text).iter().any(|c| c.code == codes::FORMAT),
                "'{good}' must be format-clean"
            );
        }
        for (bad, reason) in [
            ("PDF-Processing", "lowercase"),
            ("under_score", "lowercase letters, digits and hyphens"),
            ("with space", "lowercase letters, digits and hyphens"),
            ("café", "lowercase letters, digits and hyphens"),
            ("-pdf", "start or end with a hyphen"),
            ("pdf-", "start or end with a hyphen"),
            ("pdf--processing", "consecutive hyphens"),
        ] {
            let text = format!("---\nname: {bad}\n---\n");
            let found = checks(&text);
            let hit = found
                .iter()
                .find(|c| c.code == codes::FORMAT)
                .unwrap_or_else(|| panic!("'{bad}' must warn fm-format"));
            assert_eq!(hit.line, 1);
            assert!(hit.message.contains(reason), "'{bad}': {}", hit.message);
        }
    }

    #[test]
    fn name_rules_shared_with_dir_name_checker_agree() {
        // Parity pin: the frontmatter `name:` path (fm-format) and the
        // directory-name path (StaticAuditor / LSP dir-format) go through
        // the same skills-audit implementation — same verdict per name.
        for name in [
            "pdf-processing",
            "a",
            "42",
            "PDF-Processing",
            "under_score",
            "with space",
            "café",
            "-pdf",
            "pdf-",
            "pdf--processing",
        ] {
            let via_fm = checks(&format!("---\nname: {name}\n---\n"))
                .iter()
                .any(|c| c.code == codes::FORMAT);
            let via_dir = skills_audit::dir_name_spec_error(name).is_some();
            assert_eq!(via_fm, via_dir, "verdicts diverge for '{name}'");
        }
        // Over-length: fm path reports fm-length (via the field table), dir
        // path folds the same 64-char cap into its single verdict.
        let long = "a".repeat(65);
        assert!(
            checks(&format!("---\nname: {long}\n---\n"))
                .iter()
                .any(|c| c.code == codes::LENGTH)
        );
        assert!(skills_audit::dir_name_spec_error(&long).is_some());
    }

    #[test]
    fn bool_value_matrix() {
        for field in ["disable-model-invocation", "user-invocable"] {
            for good in ["true", "false", "\"true\""] {
                // Quotes are stripped by the reader before validation.
                let text = format!("---\n{field}: {good}\n---\n");
                assert!(checks(&text).is_empty(), "{field}: {good} must be clean");
            }
            for bad in ["yes", "no", "True", "FALSE", "1", "0"] {
                let text = format!("---\n{field}: {bad}\n---\n");
                let found = checks(&text);
                assert_eq!(found.len(), 1, "{field}: {bad}");
                assert_eq!(found[0].code, codes::VALUE);
                assert_eq!(
                    found[0].message,
                    format!("frontmatter `{field}` must be true or false (got '{bad}')")
                );
            }
        }
    }

    #[test]
    fn enum_value_matrix() {
        let cases: &[(&str, &[&str], &str)] = &[
            (
                "effort",
                &["low", "medium", "high", "xhigh", "max"],
                "ultra",
            ),
            ("context", &["fork"], "main"),
            ("shell", &["bash", "powershell"], "cmd"),
        ];
        for (field, allowed, bad) in cases {
            for good in *allowed {
                let text = format!("---\n{field}: {good}\n---\n");
                assert!(checks(&text).is_empty(), "{field}: {good} must be clean");
            }
            let text = format!("---\n{field}: {bad}\n---\n");
            let found = checks(&text);
            assert_eq!(found.len(), 1, "{field}: {bad}");
            assert_eq!(found[0].code, codes::VALUE);
            assert_eq!(
                found[0].message,
                format!(
                    "frontmatter `{field}` must be one of {} (got '{bad}')",
                    allowed.join(", ")
                )
            );
        }
    }

    #[test]
    fn empty_name_warns_empty_description_defers_to_textcheck() {
        // `name:` empty → fm-value nudge (sync falls back to the dir name).
        let found = checks("---\nname:\ndescription: d\n---\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].code, codes::VALUE);
        assert_eq!(found[0].line, 1);
        assert!(
            found[0].message.contains("directory name"),
            "{}",
            found[0].message
        );

        // Block-scalar name is absent to the reader — same nudge.
        let found = checks("---\nname: |\n  multi\ndescription: d\n---\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].code, codes::VALUE);

        // `description:` empty → nothing from here; the existing
        // `no-description` textcheck covers it (no double-reporting).
        assert!(checks("---\nname: n\ndescription:\n---\n").is_empty());
        // Empty values of other fields: nothing to validate.
        assert!(checks("---\nname: n\ndescription: d\neffort:\n---\n").is_empty());
    }

    #[test]
    fn bad_values_on_absent_lines_are_not_double_checked() {
        // A block-scalar enum value is absent to the reader → no fm-value.
        assert!(checks("---\nname: n\ndescription: d\neffort: |\n---\n").is_empty());
    }
}
