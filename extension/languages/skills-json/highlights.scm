; Adapted from Zed's canonical JSON queries (crates/grammars/src/json/highlights.scm).
(comment) @comment

(string) @string

(escape_sequence) @string.escape

(pair
  key: (string) @property.json_key)

(number) @number

[
  (true)
  (false)
] @boolean

(null) @constant.builtin

[
  ","
  ":"
] @punctuation.delimiter

[
  "{"
  "}"
  "["
  "]"
] @punctuation.bracket
