# AI Skills — Zed extension

Zed integration for the [`skills`](https://github.com/roxblnfk/zed-skills) CLI (skills manager
for AI coding agents).

What it provides:

- **Languages**: `Skills JSON` (files named `skills.json` / `skills.lock`) and `Skill Markdown`
  (files named `SKILL.md`) with syntax highlighting. Other `.json` / `.md` files are untouched.
- **Diagnostics** via the bundled language server (`skills lsp`): manifest validation, donor
  conflicts, stale lockfile, `SKILL.md` frontmatter/audit findings.
- **Code action** "Run skills update" on staleness diagnostics — applies the sync and refreshes.
- **Runnables & tasks**: ▶ gutter buttons in `skills.json` for whole-manifest and per-vendor
  sync, plus `skills: update` / `skills: update --dry-run` in the `task: spawn` palette.
  See [Runnables & tasks](#runnables--tasks) — including an important note about `skills` on PATH.

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

## Runnables & tasks

Opening a `skills.json` shows ▶ buttons in the gutter (no configuration needed — the tasks are
bundled with the extension in `languages/skills-json/tasks.json`):

- On each `remote[]` entry with a `"package"` key → **skills: update `<vendor/package>`**
  (per-vendor sync; the package is passed via `$ZED_CUSTOM_SKILLS_PACKAGE`).
- On the root `"target"` line → **skills: update** and **skills: update --dry-run**
  (whole-manifest sync).

The same tasks appear in the `task: spawn` palette whenever a `skills.json` / `skills.lock`
buffer is active (the per-vendor task only shows on its ▶, since it needs the captured package).

Not every entry gets a ▶:

- **by-url entries** (`"from": "http"` / `"zip"`) — the CLI's positional filter only accepts
  `vendor/package` patterns, not URLs.
- **GitLab subgroup packages** (`group/sub/project`, more than one `/`) — same CLI restriction.

Runnable tags (for wiring up your own tasks): `skills-sync-vendor` (per-entry, provides
`$ZED_CUSTOM_SKILLS_PACKAGE`) and `skills-sync` (whole manifest).

> **Important — tasks use `skills` from PATH.** Unlike the language server (which downloads its
> own binary), tasks run in your terminal and invoke whatever `skills` resolves to in PATH. If
> you have a *different* utility named `skills` (e.g. the PHP
> [`llm/skills`](https://github.com/roxblnfk/skills) Composer plugin), the tasks will run that
> instead. To point them at a specific binary, define project tasks in `.zed/tasks.json` with
> the **same tags** — worktree tasks take precedence over the extension's bundled ones and
> replace them in the ▶ menu. Ready-made Windows example:

```json
[
  {
    "label": "skills: update",
    "command": "C:\\tools\\skills\\skills.exe",
    "args": ["update"],
    "cwd": "$ZED_WORKTREE_ROOT",
    "tags": ["skills-sync"]
  },
  {
    "label": "skills: update --dry-run",
    "command": "C:\\tools\\skills\\skills.exe",
    "args": ["update", "--dry-run"],
    "cwd": "$ZED_WORKTREE_ROOT",
    "tags": ["skills-sync"]
  },
  {
    "label": "skills: update $ZED_CUSTOM_SKILLS_PACKAGE",
    "command": "C:\\tools\\skills\\skills.exe",
    "args": ["update", "$ZED_CUSTOM_SKILLS_PACKAGE"],
    "cwd": "$ZED_WORKTREE_ROOT",
    "tags": ["skills-sync-vendor"]
  }
]
```

On macOS/Linux replace the command with the binary's path (e.g. `~/.local/bin/skills`).

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
