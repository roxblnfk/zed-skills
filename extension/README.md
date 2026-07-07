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
  sync, plus `skills: update` / `skills: update --dry-run` / `skills: check` in the
  `task: spawn` palette.
  See [Runnables & tasks](#runnables--tasks) — including how to make the tasks run the
  extension's own binary instead of `skills` from PATH (a one-click code action).

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

The palette additionally offers **skills: check** (`skills update --check`): a read-only sync
check with compact output — it runs the full pipeline including remote ref re-resolution (so it
catches a vendor that moved while the editor was closed), writes nothing, prints one line when
in sync, and exits with the dedicated code **5** ("changes pending") plus a per-skill summary
when the target is out of sync. Conflicts and audit blocks abort with their usual codes (2 / 3).

The same tasks appear in the `task: spawn` palette whenever a `skills.json` / `skills.lock`
buffer is active (the per-vendor task only shows on its ▶, since it needs the captured package).

Not every entry gets a ▶:

- **by-url entries** (`"from": "http"` / `"zip"`) — the CLI's positional filter only accepts
  `vendor/package` patterns, not URLs.
- **GitLab subgroup packages** (`group/sub/project`, more than one `/`) — same CLI restriction.

Runnable tags (for wiring up your own tasks): `skills-sync-vendor` (per-entry, provides
`$ZED_CUSTOM_SKILLS_PACKAGE`), `skills-sync` (whole manifest) and `skills-check` (the
`--check` task).

### Which binary do the tasks run?

The *bundled* tasks run in your terminal and invoke whatever `skills` resolves to in PATH —
unlike the language server, which downloads its own binary. If you don't have this project's
CLI in PATH (or a *different* utility named `skills` shadows it, e.g. the PHP
[`llm/skills`](https://github.com/roxblnfk/skills) Composer plugin), the ▶ tasks fail or run
the wrong tool.

**Recommended fix — zero install:** open `skills.json` and run the code action
**skills: set up gutter tasks (.zed/tasks.json)** (cursor anywhere in the file →
`editor: toggle code actions`). It writes the same four tasks into your project's
`.zed/tasks.json` with `command` set to the language server's own binary — no Rust toolchain
or CLI install needed. Worktree tasks with the same tags replace the bundled ones in the ▶
menu, so the gutter buttons just start working.

How the generated file is managed:

- Entries carrying a `skills-*` tag are owned by the generator: rerunning the action refreshes
  them, and on every server start stale binary paths (a pruned versioned extension download)
  are silently repointed at the current binary — so the file survives extension updates.
- Extra fields you add to those entries (`"hooks"`, `"reveal"`, `"hide"`, …) are preserved on
  refresh; if you replace an entry's `command` with your own existing binary, the generator
  leaves the whole entry alone.
- Your other tasks and comments in the file are never touched. The action refuses to rewrite
  (with an explanatory message) rather than ever lose content.

**Manual alternative:** define the tasks yourself in `.zed/tasks.json` with the **same tags**
and the full path to any `skills` binary, e.g. on Windows:

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
  },
  {
    "label": "skills: check",
    "command": "C:\\tools\\skills\\skills.exe",
    "args": ["update", "--check"],
    "cwd": "$ZED_WORKTREE_ROOT",
    "tags": ["skills-check"]
  }
]
```

On macOS/Linux replace the command with the binary's path (e.g. `~/.local/bin/skills`).
Note: pointing an entry at a custom binary makes it yours — the code action and the startup
reconciliation will not touch it as long as that binary exists.

### Automatic sync check on worktree creation

Zed task templates support `"hooks": ["create_worktree"]` — the task runs automatically after
Zed creates a new **linked Git worktree** (the `git: create worktree` flow). Hooks are honored
**only for tasks from `.zed/tasks.json` or your global tasks.json** — Zed collects hook tasks
from file-based task settings exclusively, so the extension cannot ship this behavior in its
bundled tasks.

Easiest path: run the [code action](#which-binary-do-the-tasks-run) first, then add
`"hooks": ["create_worktree"]`, `"reveal": "no_focus"` and `"hide": "on_success"` to the
generated **skills: check** entry — the generator preserves those extra fields when it
refreshes the entry. Writing the task by hand works too:

```json
[
  {
    "label": "skills: check",
    "command": "skills",
    "args": ["update", "--check"],
    "cwd": "$ZED_WORKTREE_ROOT",
    "hooks": ["create_worktree"],
    "reveal": "no_focus",
    "hide": "on_success",
    "tags": ["skills-check"]
  }
]
```

`hide: "on_success"` closes the terminal tab when everything is in sync (exit 0); an
out-of-sync tree exits 5, so the tab stays open showing what would change and the
`run 'skills update' to apply` hint. Note the hook fires on linked-worktree creation, not on
every workspace open — for an on-demand check use the bundled **skills: check** palette task.

> **Careful with `"command": "skills"` in a hook task**: it resolves from PATH, and the PHP
> `llm/skills` tool's `update` command has **no `--check` option** — the hook would print an
> unknown-option error on *every worktree creation*. Prefer the code action (absolute path to
> the extension's binary) or a full path.

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
