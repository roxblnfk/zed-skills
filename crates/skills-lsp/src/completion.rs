//! `textDocument/completion` for the SKILL.md frontmatter block.
//!
//! Three completion surfaces, all confined to the frontmatter:
//!
//! - **bootstrap** — on line 0 of a frontmatter-less document a single
//!   snippet item inserts a whole `---\nname: …\ndescription: \n---` block;
//! - **key position** (start of a line inside the block) — known frontmatter
//!   keys not already present, inserted as `key: `;
//! - **value position** (after `key:`) — `true`/`false` for booleans, the
//!   containing directory name for `name`, fixed values for enum-ish fields.
//!
//! The field table below is the single place to extend (like the
//! danger-pattern table in skills-audit). Sources (2026-07-07):
//! - Agent Skills spec: <https://agentskills.io/specification>
//! - Zed skills docs: <https://zed.dev/docs/ai/skills>
//! - Claude Code skills reference: <https://code.claude.com/docs/en/skills>
//!
//! Frontmatter keys are lowercase and case-sensitive by ecosystem
//! convention. The sync-side reader (skills-core::frontmatter) consumes only
//! flat `key: value` lines within the first 4096 bytes — map-valued fields
//! (`metadata`, `hooks`) are still offered, with a doc note that sync treats
//! their nested values as absent (they exist for other consumers).

use tower_lsp_server::ls_types::{
    CompletionItem, CompletionItemKind, CompletionTextEdit, Documentation, InsertTextFormat,
    Position, Range, TextEdit,
};

/// How a field's value completes.
#[derive(Debug, Clone, Copy)]
pub enum ValueKind {
    /// Free-form string — no value suggestions.
    Text,
    /// `true` / `false`.
    Bool,
    /// One of a fixed set of values.
    Enum(&'static [&'static str]),
    /// Space/comma-separated string or YAML list — no value suggestions.
    List,
    /// Nested YAML map — opaque to the sync-side flat reader.
    Map,
    /// The skill name — suggests the containing directory name.
    SkillName,
}

/// One known frontmatter field. The whole vocabulary lives in
/// [`FRONTMATTER_FIELDS`] — extend it there.
pub struct FrontmatterField {
    pub key: &'static str,
    pub value: ValueKind,
    /// One-line doc shown in the completion item.
    pub doc: &'static str,
    /// Which ecosystem consumes the field: `common` (Agent Skills spec —
    /// read by both Zed and Claude Code), `zed, claude`, or `claude`.
    pub ecosystem: &'static str,
}

pub const FRONTMATTER_FIELDS: &[FrontmatterField] = &[
    FrontmatterField {
        key: "name",
        value: ValueKind::SkillName,
        doc: "Skill name: lowercase letters, digits and hyphens, max 64 chars; should match \
              the skill directory name (skills sync keys conflicts on the directory name).",
        ecosystem: "common",
    },
    FrontmatterField {
        key: "description",
        value: ValueKind::Text,
        doc: "What the skill does and when to use it (max 1024 chars) — agents use this to \
              decide when to load the skill.",
        ecosystem: "common",
    },
    FrontmatterField {
        key: "license",
        value: ValueKind::Text,
        doc: "License name or a reference to a bundled license file.",
        ecosystem: "common",
    },
    FrontmatterField {
        key: "compatibility",
        value: ValueKind::Text,
        doc: "Environment requirements (intended product, system packages, network access), \
              max 500 chars. Most skills do not need it.",
        ecosystem: "common",
    },
    FrontmatterField {
        key: "metadata",
        value: ValueKind::Map,
        doc: "Arbitrary key-value map for extra properties. Nested YAML — the skills sync \
              frontmatter reader ignores non-flat values (harmless; read by other consumers).",
        ecosystem: "common",
    },
    FrontmatterField {
        key: "allowed-tools",
        value: ValueKind::List,
        doc: "Tools the agent may use without asking while the skill is active \
              (space/comma-separated string or YAML list). Experimental in the spec.",
        ecosystem: "common",
    },
    FrontmatterField {
        key: "disable-model-invocation",
        value: ValueKind::Bool,
        doc: "true prevents the agent from loading the skill autonomously — it stays \
              invocable manually via /name.",
        ecosystem: "zed, claude",
    },
    FrontmatterField {
        key: "user-invocable",
        value: ValueKind::Bool,
        doc: "false hides the skill from the / menu (background knowledge the user should \
              not invoke directly). Default: true.",
        ecosystem: "claude",
    },
    FrontmatterField {
        key: "when_to_use",
        value: ValueKind::Text,
        doc: "Extra trigger context (phrases, example requests) appended to the description \
              in the skill listing.",
        ecosystem: "claude",
    },
    FrontmatterField {
        key: "argument-hint",
        value: ValueKind::Text,
        doc: "Autocomplete hint for expected arguments, e.g. [issue-number] or \
              [filename] [format].",
        ecosystem: "claude",
    },
    FrontmatterField {
        key: "arguments",
        value: ValueKind::List,
        doc: "Named positional arguments for $name substitution in the skill body \
              (space-separated string or YAML list).",
        ecosystem: "claude",
    },
    FrontmatterField {
        key: "disallowed-tools",
        value: ValueKind::List,
        doc: "Tools removed from the agent's pool while the skill is active \
              (space/comma-separated string or YAML list).",
        ecosystem: "claude",
    },
    FrontmatterField {
        key: "model",
        value: ValueKind::Text,
        doc: "Model override while the skill is active (same values as /model, or 'inherit').",
        ecosystem: "claude",
    },
    FrontmatterField {
        key: "effort",
        value: ValueKind::Enum(&["low", "medium", "high", "xhigh", "max"]),
        doc: "Effort-level override while the skill is active.",
        ecosystem: "claude",
    },
    FrontmatterField {
        key: "context",
        value: ValueKind::Enum(&["fork"]),
        doc: "'fork' runs the skill in an isolated subagent context (the body becomes the \
              subagent prompt).",
        ecosystem: "claude",
    },
    FrontmatterField {
        key: "agent",
        value: ValueKind::Text,
        doc: "Subagent type to use when 'context: fork' is set.",
        ecosystem: "claude",
    },
    FrontmatterField {
        key: "hooks",
        value: ValueKind::Map,
        doc: "Hooks scoped to the skill's lifecycle. Nested YAML — the skills sync \
              frontmatter reader ignores non-flat values (harmless; read by other consumers).",
        ecosystem: "claude",
    },
    FrontmatterField {
        key: "paths",
        value: ValueKind::List,
        doc: "Glob patterns limiting when the skill auto-activates \
              (comma-separated string or YAML list).",
        ecosystem: "claude",
    },
    FrontmatterField {
        key: "shell",
        value: ValueKind::Enum(&["bash", "powershell"]),
        doc: "Shell for inline shell blocks in the skill body: bash (default) or powershell.",
        ecosystem: "claude",
    },
];

/// Completion items for a SKILL.md buffer at an LSP position (UTF-16
/// `character`). `dir_name` is the containing directory (the skill id) when
/// known. Empty result = nothing to offer here.
pub fn complete(
    text: &str,
    line: u32,
    character: u32,
    dir_name: Option<&str>,
) -> Vec<CompletionItem> {
    let lines: Vec<&str> = text.lines().map(|l| l.trim_end_matches('\r')).collect();
    let opened = lines.first() == Some(&"---");
    let closing = opened
        .then(|| {
            lines
                .iter()
                .skip(1)
                .position(|l| *l == "---")
                .map(|i| i + 1)
        })
        .flatten();

    // Bootstrap: line 0 of a document without a complete frontmatter block,
    // with nothing but dashes typed before the cursor.
    if line == 0 && character <= 3 && closing.is_none() {
        let line0 = lines.first().copied().unwrap_or("");
        let prefix = utf16_prefix(line0, character);
        if prefix.chars().all(|c| c == '-') {
            return vec![bootstrap_item(character, dir_name)];
        }
    }

    // Everything else lives strictly inside the frontmatter block: after the
    // byte-0 `---`, before the closing `---` (an unclosed block — the user is
    // mid-typing — completes up to the end of the document).
    let in_block = opened && line >= 1 && closing.is_none_or(|c| (line as usize) < c);
    if !in_block {
        return Vec::new();
    }

    let cursor_line = lines.get(line as usize).copied().unwrap_or("");
    let prefix = utf16_prefix(cursor_line, character);

    match prefix.split_once(':') {
        Some((key, after)) => value_items(key.trim_end(), after, dir_name),
        None => key_items(&lines, closing, line as usize),
    }
}

/// The `---\nname: …` block snippet offered on line 0.
fn bootstrap_item(character: u32, dir_name: Option<&str>) -> CompletionItem {
    let name = dir_name.unwrap_or("skill-name");
    let snippet = format!("---\nname: {name}\ndescription: $1\n---\n$0");
    CompletionItem {
        label: "--- (skill frontmatter)".to_string(),
        kind: Some(CompletionItemKind::SNIPPET),
        detail: Some("frontmatter block".to_string()),
        documentation: Some(Documentation::String(
            "Insert a SKILL.md frontmatter block with name and description.".to_string(),
        )),
        filter_text: Some("---".to_string()),
        insert_text_format: Some(InsertTextFormat::SNIPPET),
        // Replace the dashes already typed on line 0.
        text_edit: Some(CompletionTextEdit::Edit(TextEdit {
            range: Range::new(Position::new(0, 0), Position::new(0, character)),
            new_text: snippet,
        })),
        ..CompletionItem::default()
    }
}

/// Key-position items: every known field whose key is not already present in
/// the block (the cursor's own line does not count as present).
fn key_items(lines: &[&str], closing: Option<usize>, cursor_line: usize) -> Vec<CompletionItem> {
    let end = closing.unwrap_or(lines.len());
    let present: Vec<&str> = lines[1..end.min(lines.len())]
        .iter()
        .enumerate()
        .filter(|(idx, _)| idx + 1 != cursor_line)
        .filter(|(_, l)| !l.starts_with([' ', '\t']))
        .filter_map(|(_, l)| l.split_once(':'))
        .map(|(key, _)| key.trim_end())
        .collect();

    FRONTMATTER_FIELDS
        .iter()
        .enumerate()
        .filter(|(_, field)| !present.contains(&field.key))
        .map(|(idx, field)| CompletionItem {
            label: field.key.to_string(),
            kind: Some(CompletionItemKind::PROPERTY),
            detail: Some(value_detail(field.value)),
            documentation: Some(Documentation::String(format!(
                "[{}] {}",
                field.ecosystem, field.doc
            ))),
            insert_text: Some(format!("{}: ", field.key)),
            // Keep the table order (common fields first).
            sort_text: Some(format!("{idx:02}")),
            ..CompletionItem::default()
        })
        .collect()
}

/// Value-position items for `key:` — booleans, enum values, the directory
/// name for `name`. Free-form fields yield nothing.
fn value_items(key: &str, after_colon: &str, dir_name: Option<&str>) -> Vec<CompletionItem> {
    let Some(field) = FRONTMATTER_FIELDS.iter().find(|f| f.key == key) else {
        return Vec::new();
    };
    let values: Vec<String> = match field.value {
        ValueKind::Bool => vec!["true".to_string(), "false".to_string()],
        ValueKind::Enum(values) => values.iter().map(|v| (*v).to_string()).collect(),
        ValueKind::SkillName => dir_name.map(str::to_string).into_iter().collect(),
        ValueKind::Text | ValueKind::List | ValueKind::Map => Vec::new(),
    };
    values
        .into_iter()
        .enumerate()
        .map(|(idx, value)| {
            // Right after the bare colon, complete the separating space too.
            let insert = if after_colon.is_empty() {
                Some(format!(" {value}"))
            } else {
                None // label inserted verbatim
            };
            CompletionItem {
                label: value,
                kind: Some(CompletionItemKind::VALUE),
                detail: Some(format!("{key} value")),
                insert_text: insert,
                sort_text: Some(format!("{idx:02}")),
                ..CompletionItem::default()
            }
        })
        .collect()
}

fn value_detail(value: ValueKind) -> String {
    match value {
        ValueKind::Text | ValueKind::SkillName => "string".to_string(),
        ValueKind::Bool => "boolean".to_string(),
        ValueKind::Enum(values) => values.join(" | "),
        ValueKind::List => "list (string or YAML list)".to_string(),
        ValueKind::Map => "map (nested YAML)".to_string(),
    }
}

/// The slice of `line` before a UTF-16 column (LSP positions are UTF-16).
fn utf16_prefix(line: &str, character: u32) -> &str {
    let mut units = 0u32;
    for (idx, ch) in line.char_indices() {
        if units >= character {
            return &line[..idx];
        }
        units += ch.len_utf16() as u32;
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(items: &[CompletionItem]) -> Vec<&str> {
        items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn key_position_excludes_present_keys() {
        let text = "---\nname: x\ndescription: d\n\n---\nbody\n";
        let items = complete(text, 3, 0, Some("x"));
        let labels = labels(&items);
        assert!(!labels.contains(&"name"));
        assert!(!labels.contains(&"description"));
        assert!(labels.contains(&"license"));
        assert!(labels.contains(&"disable-model-invocation"));
        assert_eq!(items.len(), FRONTMATTER_FIELDS.len() - 2);
    }

    #[test]
    fn key_position_inserts_trailing_space() {
        let text = "---\n\n---\n";
        let items = complete(text, 1, 0, None);
        let license = items.iter().find(|i| i.label == "license").unwrap();
        assert_eq!(license.insert_text.as_deref(), Some("license: "));
        assert_eq!(license.kind, Some(CompletionItemKind::PROPERTY));
        let Some(Documentation::String(doc)) = &license.documentation else {
            panic!("string documentation expected");
        };
        assert!(doc.starts_with("[common]"), "{doc}");
    }

    #[test]
    fn cursor_own_line_does_not_count_as_present() {
        // The user is retyping `desc` on a line that parses as key-less.
        let text = "---\nname: x\ndesc\n---\n";
        let items = complete(text, 2, 4, Some("x"));
        assert!(labels(&items).contains(&"description"));
        assert!(!labels(&items).contains(&"name"));
    }

    #[test]
    fn unclosed_block_still_completes_keys() {
        let text = "---\n";
        let items = complete(text, 1, 0, None);
        assert_eq!(items.len(), FRONTMATTER_FIELDS.len());
    }

    #[test]
    fn value_position_bool_and_enums() {
        let text = "---\ndisable-model-invocation: \n---\n";
        let items = complete(text, 1, 26, None);
        assert_eq!(labels(&items), ["true", "false"]);
        assert_eq!(items[0].kind, Some(CompletionItemKind::VALUE));
        // Cursor after the space → label inserted verbatim.
        assert_eq!(items[0].insert_text, None);

        let text = "---\neffort:\n---\n";
        let items = complete(text, 1, 7, None);
        assert_eq!(labels(&items), ["low", "medium", "high", "xhigh", "max"]);
        // Right after the bare colon → the separating space is completed too.
        assert_eq!(items[0].insert_text.as_deref(), Some(" low"));

        let text = "---\nshell: \n---\n";
        assert_eq!(labels(&complete(text, 1, 7, None)), ["bash", "powershell"]);
    }

    #[test]
    fn name_value_suggests_dir_name() {
        let text = "---\nname: \n---\n";
        assert_eq!(labels(&complete(text, 1, 6, Some("deploy"))), ["deploy"]);
        assert!(complete(text, 1, 6, None).is_empty());
    }

    #[test]
    fn free_form_and_unknown_values_offer_nothing() {
        let text = "---\ndescription: \nwat: \n---\n";
        assert!(complete(text, 1, 13, None).is_empty());
        assert!(complete(text, 2, 5, None).is_empty());
    }

    #[test]
    fn bootstrap_on_empty_and_partial_dashes() {
        let items = complete("", 0, 0, Some("deploy"));
        assert_eq!(items.len(), 1);
        let Some(CompletionTextEdit::Edit(edit)) = &items[0].text_edit else {
            panic!("text edit expected");
        };
        assert!(edit.new_text.starts_with("---\nname: deploy\n"));
        assert_eq!(
            edit.range,
            Range::new(Position::new(0, 0), Position::new(0, 0))
        );

        let items = complete("--", 0, 2, None);
        assert_eq!(items.len(), 1);
        assert!(items[0].label.starts_with("---"));
        let Some(CompletionTextEdit::Edit(edit)) = &items[0].text_edit else {
            panic!("text edit expected");
        };
        assert_eq!(edit.range.end, Position::new(0, 2));
        assert!(edit.new_text.contains("name: skill-name"));
    }

    #[test]
    fn no_bootstrap_when_frontmatter_exists() {
        let text = "---\nname: x\n---\nbody\n";
        assert!(complete(text, 0, 0, Some("x")).is_empty());
        assert!(complete(text, 0, 3, Some("x")).is_empty());
    }

    #[test]
    fn no_completion_outside_frontmatter() {
        let text = "---\nname: x\n---\nbody\n";
        assert!(complete(text, 3, 0, Some("x")).is_empty()); // body
        assert!(complete(text, 2, 0, Some("x")).is_empty()); // closing line
        assert!(complete("# title\n", 0, 7, None).is_empty()); // no block, col > 3
        assert!(complete("# title\nname\n", 1, 0, None).is_empty());
    }

    #[test]
    fn utf16_positions_in_values() {
        // '🎉' is 2 UTF-16 units; the colon split must still see the key.
        let text = "---\ndescription: 🎉 party\nname: \n---\n";
        assert!(complete(text, 1, 16, None).is_empty());
        assert_eq!(labels(&complete(text, 2, 6, Some("p"))), ["p"]);
    }
}
