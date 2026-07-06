//! JSON path → LSP range resolution over the raw `skills.json` buffer text.
//!
//! Built on `jsonc-parser` (dprint), whose AST carries byte ranges for every
//! node. Paths use [`skills_core::manifest::PathSeg`], the same segments the
//! manifest validator anchors its issues with. A path that cannot be
//! resolved (buffer edited mid-analysis, malformed JSON, …) falls back to
//! the document's first line.

use jsonc_parser::CollectOptions;
use jsonc_parser::ParseOptions;
use jsonc_parser::ast::{ObjectPropName, Value};
use jsonc_parser::common::Ranged;
use skills_core::manifest::PathSeg;
use tower_lsp_server::ls_types::{Position, Range};

/// Byte-offset → UTF-16 position index plus the parsed JSON AST of one
/// document snapshot.
pub struct SpanIndex<'a> {
    text: &'a str,
    /// Byte offset of the start of every line.
    line_starts: Vec<usize>,
    ast: Option<Value<'a>>,
}

impl<'a> SpanIndex<'a> {
    pub fn new(text: &'a str) -> Self {
        let mut line_starts = vec![0usize];
        for (idx, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(idx + 1);
            }
        }
        let ast =
            jsonc_parser::parse_to_ast(text, &CollectOptions::default(), &ParseOptions::default())
                .ok()
                .and_then(|result| result.value);
        SpanIndex {
            text,
            line_starts,
            ast,
        }
    }

    /// UTF-16 LSP position of a byte offset (clamped to the text length).
    pub fn position(&self, offset: usize) -> Position {
        let offset = offset.min(self.text.len());
        let line = self.line_starts.partition_point(|&start| start <= offset) - 1;
        let col = self.text[self.line_starts[line]..offset]
            .encode_utf16()
            .count();
        Position::new(line as u32, col as u32)
    }

    /// Full-line range for a 0-based line index (clamped to the last line).
    pub fn line_range(&self, line: usize) -> Range {
        let line = line.min(self.line_starts.len() - 1);
        let start = self.line_starts[line];
        let end = match self.line_starts.get(line + 1) {
            Some(&next) => next - 1,
            None => self.text.len(),
        };
        Range::new(self.position(start), self.position(end))
    }

    /// The fallback anchor: the whole first line of the document.
    pub fn first_line(&self) -> Range {
        self.line_range(0)
    }

    /// Range of the value at `path`, or the first line when the path cannot
    /// be resolved.
    pub fn range_or_first_line(&self, path: &[PathSeg]) -> Range {
        self.range_of(path).unwrap_or_else(|| self.first_line())
    }

    /// Range of the value at `path` (array elements addressed by index).
    pub fn range_of(&self, path: &[PathSeg]) -> Option<Range> {
        let mut node = self.ast.as_ref()?;
        for seg in path {
            node = match seg {
                PathSeg::Key(key) => {
                    let prop = node
                        .as_object()?
                        .properties
                        .iter()
                        .find(|p| prop_name(&p.name) == key)?;
                    &prop.value
                }
                PathSeg::Index(idx) => node.as_array()?.elements.get(*idx)?,
            };
        }
        let range = node.range();
        Some(Range::new(
            self.position(range.start),
            self.position(range.end),
        ))
    }

    /// Range for a 1-based (line, column) pair as reported by
    /// `serde_json::Error` — anchored from that point to the end of the line.
    pub fn error_range(&self, line: usize, column: usize) -> Range {
        let line0 = line.saturating_sub(1).min(self.line_starts.len() - 1);
        let full = self.line_range(line0);
        let col0 = (column.saturating_sub(1) as u32).min(full.end.character);
        let start = Position::new(full.start.line, col0.max(full.start.character));
        if start.character >= full.end.character {
            // Point at the whole line when the column is at/past its end.
            full
        } else {
            Range::new(start, full.end)
        }
    }
}

fn prop_name<'a>(name: &'a ObjectPropName<'a>) -> &'a str {
    name.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(k: &str) -> PathSeg {
        PathSeg::key(k)
    }

    const DOC: &str = r#"{
  "target": ".agents/skills",
  "aliases": [".claude/skills", ".cursor/skills"],
  "local": { "dir": ["./skills-src"] },
  "remote": [
    { "from": "github", "package": "acme/skills",
      "skills": ["one", "two"] },
    { "from": "zip", "url": "https://example.com/a.zip" }
  ]
}"#;

    #[test]
    fn resolves_top_level_key_to_value_range() {
        let span = SpanIndex::new(DOC);
        let range = span.range_of(&[key("target")]).unwrap();
        assert_eq!(range.start, Position::new(1, 12));
        assert_eq!(range.end, Position::new(1, 28));
    }

    #[test]
    fn resolves_array_elements() {
        let span = SpanIndex::new(DOC);
        let range = span.range_of(&[key("aliases"), PathSeg::Index(1)]).unwrap();
        assert_eq!(range.start.line, 2);
        let text_at = &DOC.lines().nth(2).unwrap()
            [range.start.character as usize..range.end.character as usize];
        assert_eq!(text_at, "\".cursor/skills\"");
    }

    #[test]
    fn resolves_nested_object_and_array_paths() {
        let span = SpanIndex::new(DOC);
        let range = span
            .range_of(&[key("local"), key("dir"), PathSeg::Index(0)])
            .unwrap();
        assert_eq!(range.start.line, 3);

        // remote[0] spans the whole entry object (multi-line).
        let range = span.range_of(&[key("remote"), PathSeg::Index(0)]).unwrap();
        assert_eq!(range.start.line, 5);
        assert_eq!(range.end.line, 6);

        // remote[0].skills[1] is the exact allowlist element.
        let range = span
            .range_of(&[
                key("remote"),
                PathSeg::Index(0),
                key("skills"),
                PathSeg::Index(1),
            ])
            .unwrap();
        assert_eq!(range.start.line, 6);
        let text_at = &DOC.lines().nth(6).unwrap()
            [range.start.character as usize..range.end.character as usize];
        assert_eq!(text_at, "\"two\"");
    }

    #[test]
    fn unresolvable_path_falls_back_to_first_line() {
        let span = SpanIndex::new(DOC);
        assert!(span.range_of(&[key("nope")]).is_none());
        assert!(span.range_of(&[key("remote"), PathSeg::Index(9)]).is_none());
        // Key lookup into an array does not panic.
        assert!(span.range_of(&[key("remote"), key("x")]).is_none());
        let fallback = span.range_or_first_line(&[key("nope")]);
        assert_eq!(fallback.start, Position::new(0, 0));
        assert_eq!(fallback.end, Position::new(0, 1));
    }

    #[test]
    fn malformed_json_yields_no_ranges_but_first_line_works() {
        let span = SpanIndex::new("{ not json at all");
        assert!(span.range_of(&[key("target")]).is_none());
        assert_eq!(span.first_line().start, Position::new(0, 0));
    }

    #[test]
    fn utf16_columns_for_non_ascii_lines() {
        // '🦀' is 4 UTF-8 bytes but 2 UTF-16 code units.
        let doc = "{ \"🦀\": \"v\" }";
        let span = SpanIndex::new(doc);
        let range = span.range_of(&[key("🦀")]).unwrap();
        // Value starts after `{ "🦀": ` = 2 + 4 (`"🦀"`) + 2 = position 8.
        assert_eq!(range.start, Position::new(0, 8));
    }

    #[test]
    fn error_range_points_from_column_to_line_end() {
        let doc = "{\n  \"target\": 5,\n}";
        let span = SpanIndex::new(doc);
        let range = span.error_range(2, 13);
        assert_eq!(range.start, Position::new(1, 12));
        assert_eq!(range.end.line, 1);
        // Out-of-bounds lines clamp instead of panicking.
        let clamped = span.error_range(99, 1);
        assert_eq!(clamped.start.line, 2);
    }
}
