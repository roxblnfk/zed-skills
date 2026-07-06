# AI Skills — Zed extension

Zed integration for the [`skills`](https://github.com/roxblnfk/zed-skills) CLI (skills manager
for AI coding agents).

What it provides:

- **Languages**: `Skills JSON` (files named `skills.json` / `skills.lock`) and `Skill Markdown`
  (files named `SKILL.md`) with syntax highlighting. Other `.json` / `.md` files are untouched.
- **Diagnostics** via the bundled language server (`skills lsp`): manifest validation, donor
  conflicts, stale lockfile, `SKILL.md` frontmatter/audit findings.
- **Code action** "Run skills update" on staleness diagnostics — applies the sync and refreshes.

The server starts when a `skills.json`, `skills.lock`, or `SKILL.md` file is opened in a trusted
worktree.

## How the `skills` binary is resolved

In this order:

1. **Settings override** — `lsp.skills.binary.path` in Zed settings (always wins).
2. The binary **previously downloaded** by this extension.
3. **Download** of the latest stable release from
   [`roxblnfk/zed-skills` releases](https://github.com/roxblnfk/zed-skills/releases)
   (prebuilt for Windows x86_64, macOS aarch64, Linux x86_64/aarch64), stored under a versioned
   directory in the extension's work dir; older versions are pruned.
4. `skills` on **PATH** — last resort only, for platforms without a prebuilt asset
   (e.g. x86_64 macOS) or when the download fails.

> **Note:** if you have *another* utility named `skills` in PATH (e.g. the PHP
> [`llm/skills`](https://github.com/roxblnfk/skills) Composer plugin), the extension will **not**
> use it — it downloads its own binary. Use the settings override to point at a specific binary
> if needed:

```json
{
  "lsp": {
    "skills": {
      "binary": {
        "path": "C:\\tools\\skills\\skills.exe",
        "arguments": ["lsp"]
      }
    }
  }
}
```

On x86_64 macOS there is no prebuilt asset: install the `skills` CLI into PATH (or set the
settings override above) and the extension will use it.

## Developing / installing as a dev extension

Prerequisites: Rust via **rustup** (not homebrew/other) and the wasm target:

```
rustup target add wasm32-wasip2
```

Then in Zed:

1. Command palette → `zed: extensions`.
2. Click **Install Dev Extension**.
3. Pick this `extension/` directory. Zed compiles the extension (wasm32-wasip2) and the two
   grammars itself.
4. Open a `skills.json` or `SKILL.md` — the `Skills Language Server` should appear in the
   language server logs; the binary is downloaded on first start.

This crate is intentionally **not** part of the repository's root cargo workspace (it targets
wasm). To build/lint manually:

```
cd extension
cargo build --release --target wasm32-wasip2
cargo clippy --release --target wasm32-wasip2 -- -D warnings
```
