# ai-skills

[![GitHub release](https://img.shields.io/github/v/release/roxblnfk/zed-skills?style=flat-square)](https://github.com/roxblnfk/zed-skills/releases/latest)
[![Vibe Index](https://img.shields.io/static/v1?label=Vibe+Index&message=8.0&color=7252e6&style=flat-square&logo=data%3Aimage%2Fsvg%2Bxml%3Bbase64%2CPHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCAyNCAyNCIgZmlsbD0iI2ZmZiI%2BPHBhdGggZD0iTTkgNCBROSAxMyAxOCAxMyBROSAxMyA5IDIyIFE5IDEzIDAgMTMgUTkgMTMgOSA0IFoiLz48cGF0aCBkPSJNMTkgMSBRMTkgNiAyNCA2IFExOSA2IDE5IDExIFExOSA2IDE0IDYgUTE5IDYgMTkgMSBaIi8%2BPHBhdGggZD0iTTIwIDE0IFEyMCAxOCAyNCAxOCBRMjAgMTggMjAgMjIgUTIwIDE4IDE2IDE4IFEyMCAxOCAyMCAxNCBaIi8%2BPC9zdmc%2B)](https://github.com/roxblnfk/action-vibe-index)
![License](https://img.shields.io/badge/license-BSD--3--Clause-blue?style=flat-square)

CLI skills manager for AI coding agents — primary consumer: the [Zed](https://zed.dev) editor.

`ai-skills` downloads and syncs **AI Skills** from vendors (local dependency
directories and remote GitHub/GitLab repositories) into a project-local
directory, so your agents always have the right skills on hand. It is a Rust
rewrite of the PHP Composer plugin [`llm/skills`](https://github.com/roxblnfk/skills)
— same idea, refactored architecture.

## What is a skill?

A **skill** is a directory with a `SKILL.md` at its root, optionally
accompanied by `scripts/`, `references/`, and `assets/`. The directory name is
the skill id.

Skills are synced into `.agents/skills/` by default (Zed reads it natively);
`aliases` mirror the tree to `.claude/skills/` and other agent locations.

## Usage

`ai-skills` works both as a standalone CLI binary and as a Zed extension.
Project configuration lives in `skills.json` at the project root (schema v2).

### As a standalone binary

**Install** — download or build:

- **Download** (recommended): grab the prebuilt binary for your platform from
  the [latest release](https://github.com/roxblnfk/zed-skills/releases/latest),
  unpack the archive, and put `skills` on your `PATH`.
- **Build from source** ![MSRV](https://img.shields.io/badge/rustc-1.96+-blue?style=flat-square):

  ```bash
  cargo install --path crates/skills-cli
  ```

**Use.**

```bash
# Scaffold a skills.json manifest
skills init

# Preview what a sync would do, without touching the filesystem
skills update --dry-run

# Sync skills declared in skills.json into .agents/skills/
skills update
```

### As a Zed extension

> 🚧 **Planned.** Installation and usage as a Zed extension will be documented
> here once the extension packaging and entry point are finalized.

<!-- TODO(zed-extension): document installation via the Zed extensions
     registry, required configuration, and how the extension drives the sync
     pipeline. -->


## Commands

| Command | Description |
| --- | --- |
| `skills init` | Scaffold a `skills.json` manifest |
| `skills update` | Discover, audit and sync skills into the target directory |
| `skills show` | Show resolved skills and their audit annotations |
| `skills add` | Add a vendor/package to the manifest |

Most commands support `--dry-run`. See `skills --help` for the full set.

## Architecture

A typed pipeline where the only writing stage is the last one
(fail-before-FS-touch):

```
Prepare → Discover (par) → TrustFilter → Materialize (par) → Locate+Scan (par)
        → Resolve (barrier) → Audit (par) → Plan → Sync (transactional)
```

## Workspace layout

| Crate | Responsibility |
| --- | --- |
| `skills-core` | Domain, manifest, lockfile, pipeline stages, traits (no network/CLI) |
| `skills-providers` | Dir, Composer, GitHub, GitLab providers; HTTP client abstraction |
| `skills-audit` | `StaticAuditor` and (later) LLM/HTTP auditors |
| `skills-cli` | The `skills` binary (`init`/`update`/`show`/`add`, `--dry-run`, …) |

## Development

[![Tests](https://github.com/roxblnfk/zed-skills/actions/workflows/tests.yml/badge.svg)](https://github.com/roxblnfk/zed-skills/actions/workflows/tests.yml)
[![Dependencies](https://deps.rs/repo/github/roxblnfk/zed-skills/status.svg)](https://deps.rs/repo/github/roxblnfk/zed-skills)

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```
