//! `textDocument/hover` for SKILL.md frontmatter keys.
//!
//! Hovering a known `key:` line inside the frontmatter block shows the
//! field's one-line doc from the [`FRONTMATTER_FIELDS`] table (the same
//! single source of truth completion and validation use), formatted as
//! Markdown:
//!
//! ```text
//! **effort** _(enum: low | medium | high | xhigh | max)_ — Effort-level …
//!
//! Ecosystems: claude
//! ```
//!
//! Block/line detection is [`skills_core::frontmatter::flat_entries`] — the
//! sync-side reader's exact rules, shared with [`crate::fmcheck`] (so an
//! unclosed block, indented lines or malformed keys yield no hover, exactly
//! as they yield no validation). **Position policy (pinned by tests):** the
//! whole `key:` prefix of the line hits — the key token, any padding before
//! the colon, and the colon itself; positions inside the value miss. The
//! returned range always highlights just the key token.
//!
//! Unknown keys, positions outside the block, and `skills.json` documents
//! (filtered in the server handler) get no hover.

use tower_lsp_server::ls_types::{
    Hover, HoverContents, MarkupContent, MarkupKind, Position, Range,
};

use skills_core::frontmatter::flat_entries;

use crate::completion::{FRONTMATTER_FIELDS, FrontmatterField, ValueKind};

/// Hover for a SKILL.md buffer at an LSP position (UTF-16 `character`).
pub fn hover(text: &str, line: u32, character: u32) -> Option<Hover> {
    let entry = flat_entries(text.as_bytes())
        .into_iter()
        .find(|e| e.line == line as usize)?;
    let field = FRONTMATTER_FIELDS.iter().find(|f| f.key == entry.key)?;

    // The raw line: frontmatter keys start at column 0 and are ASCII
    // (flat_entries guarantees both), so byte offsets equal UTF-16 columns
    // up to and including the colon.
    let raw = text.lines().nth(line as usize)?.trim_end_matches('\r');
    let colon = raw.find(':')? as u32;
    if character > colon {
        return None; // inside the value — the key's docs do not apply
    }

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: field_markdown(field),
        }),
        range: Some(Range::new(
            Position::new(line, 0),
            Position::new(line, entry.key.len() as u32),
        )),
    })
}

/// `**key** _(type[, enum values][, max N chars])_ — doc` + ecosystem line.
fn field_markdown(field: &FrontmatterField) -> String {
    let mut kind = match field.value {
        ValueKind::Text | ValueKind::SkillName => "string".to_string(),
        ValueKind::Bool => "boolean".to_string(),
        ValueKind::List => "list".to_string(),
        ValueKind::Map => "map".to_string(),
        ValueKind::Enum(values) => format!("enum: {}", values.join(" | ")),
    };
    if let Some(max) = field.max_len {
        kind.push_str(&format!(", max {max} chars"));
    }
    format!(
        "**{}** _({kind})_ — {}\n\nEcosystems: {}",
        field.key, field.doc, field.ecosystem
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn markdown(hover: &Hover) -> &str {
        match &hover.contents {
            HoverContents::Markup(markup) => {
                assert_eq!(markup.kind, MarkupKind::Markdown);
                &markup.value
            }
            other => panic!("markup contents expected, got {other:?}"),
        }
    }

    const DOC: &str = "---\nname: deploy\ndisable-model-invocation: true\n\
                       effort: high\nmetadata:\n  team: x\nwat: ?\n---\nbody name: x\n";

    #[test]
    fn bool_field_formats_type_and_ecosystems() {
        let hover = hover(DOC, 2, 0).unwrap();
        let md = markdown(&hover);
        assert!(
            md.starts_with("**disable-model-invocation** _(boolean)_ — true prevents"),
            "{md}"
        );
        assert!(md.ends_with("\n\nEcosystems: zed, claude"), "{md}");
    }

    #[test]
    fn enum_field_lists_values() {
        let hover = hover(DOC, 3, 0).unwrap();
        let md = markdown(&hover);
        assert!(
            md.starts_with("**effort** _(enum: low | medium | high | xhigh | max)_ — "),
            "{md}"
        );
        assert!(md.contains("Ecosystems: claude"), "{md}");
    }

    #[test]
    fn map_field_carries_the_flat_reader_note() {
        let hover = hover(DOC, 4, 0).unwrap();
        let md = markdown(&hover);
        assert!(md.starts_with("**metadata** _(map)_ — "), "{md}");
        assert!(
            md.contains("frontmatter reader ignores non-flat values"),
            "{md}"
        );
    }

    #[test]
    fn max_len_rendered_for_limited_fields() {
        let md = markdown(&hover(DOC, 1, 0).unwrap()).to_string();
        assert!(
            md.starts_with("**name** _(string, max 64 chars)_ — "),
            "{md}"
        );
    }

    #[test]
    fn range_is_the_key_token() {
        let hover = hover(DOC, 2, 10).unwrap();
        assert_eq!(
            hover.range,
            Some(Range::new(
                Position::new(2, 0),
                Position::new(2, "disable-model-invocation".len() as u32)
            ))
        );
    }

    #[test]
    fn key_padding_and_colon_hit_value_misses() {
        // Pinned policy: everything up to and including the colon hits …
        assert!(hover(DOC, 1, 0).is_some()); // key start
        assert!(hover(DOC, 1, 3).is_some()); // inside the key
        assert!(hover(DOC, 1, 4).is_some()); // the colon itself
        // … the value does not.
        assert!(hover(DOC, 1, 5).is_none()); // space before the value
        assert!(hover(DOC, 1, 8).is_none()); // inside the value
    }

    #[test]
    fn unknown_key_and_nested_lines_miss() {
        assert!(hover(DOC, 6, 0).is_none()); // `wat:` — unknown key
        assert!(hover(DOC, 5, 2).is_none()); // indented nested line
    }

    #[test]
    fn outside_the_block_misses() {
        assert!(hover(DOC, 0, 0).is_none()); // opening ---
        assert!(hover(DOC, 7, 0).is_none()); // closing ---
        assert!(hover(DOC, 8, 5).is_none()); // body, even with `name:` text
        assert!(hover(DOC, 99, 0).is_none()); // past the document
    }

    #[test]
    fn unclosed_block_and_no_frontmatter_miss() {
        // Reader semantics: no closing --- = no frontmatter = no hover.
        assert!(hover("---\nname: x\n", 1, 0).is_none());
        assert!(hover("# title\nname: x\n", 1, 0).is_none());
    }
}
