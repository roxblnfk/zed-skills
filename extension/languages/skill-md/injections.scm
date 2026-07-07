; Verbatim copy of Zed's canonical markdown injections
; (crates/grammars/src/markdown/injections.scm) for the same pinned grammar
; rev. The `minus_metadata` → yaml rule is what colors the SKILL.md
; frontmatter; the rest lights up fenced code blocks, inline markdown,
; tables and raw HTML in the skill body.

(fenced_code_block
  (info_string
    (language) @injection.language)
  (code_fence_content) @injection.content)

((inline) @injection.content
  (#set! injection.language "markdown-inline"))

((pipe_table_cell) @injection.content
  (#set! injection.language "markdown-inline"))

((html_block) @injection.content
  (#set! injection.language "html"))

((minus_metadata) @injection.content
  (#set! injection.language "yaml"))

((plus_metadata) @injection.content
  (#set! injection.language "toml"))
