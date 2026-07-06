# ai-skills

CLI skills manager for AI coding agents (primary consumer: **Zed editor**). Rust rewrite of the PHP Composer plugin [`llm/skills`](https://github.com/roxblnfk/skills) — same idea, refactored architecture: download/sync AI Skills from vendors (local dependency dirs and remote GitHub/GitLab repos) into a project-local directory.

- A **skill** = a directory with `SKILL.md` at its root (+ optional `scripts/`, `references/`, `assets/`). Directory name = skill id.
- Default sync target: `.agents/skills/` — Zed reads it natively; `aliases` mirror it to `.claude/skills/` etc.
- Manifest: `skills.json` at project root (schema v2, inspired by the PHP package but not backward-compatible).
- Reference PHP implementation lives locally at `D:\git\llm-agents\skills` — consult it for acceptance-test scenarios and provider contracts (GitLab has tricky subgroup/nested-project handling).

## Documents

- `docs/PLAN.md` — milestone plan and current progress. **Not committed** (excluded via `.git/info/exclude`), keep it updated as work proceeds.
- `docs/SPEC.md` — technical spec: pipeline stages, trait contracts, manifest/lockfile formats, audit chain, test strategy. **Not committed.** Read it before implementing anything.

## Architecture (short)

Typed pipeline; the only writing stage is the last one (fail-before-FS-touch):

```
Prepare → Discover (par) → TrustFilter → Materialize (par) → Locate+Scan (par)
        → Resolve (barrier) → Audit (par) → Plan → Sync (transactional)
```

Key contracts (traits in `skills-core`): `VendorProvider` (discover donors), `Vendor::materialize()` (local and remote vendors become indistinguishable — a dir on disk), `SkillLocator` (Declared → WellKnown → RecursiveFallback chain), `Auditor` (configurable chain from `skills.json`).

## Workspace layout

```
crates/skills-core/       # domain, manifest, lockfile, pipeline stages, traits. No network/CLI.
crates/skills-providers/  # Dir, Composer, GitHub, GitLab providers; HTTP client abstraction
crates/skills-audit/      # StaticAuditor, (later) LLM/HTTP auditors
crates/skills-lsp/        # `skills lsp`: diagnostics server for skills.json/SKILL.md (stdio, notify watcher)
crates/skills-cli/        # clap binary `skills`: init/update/show/add/lsp, --dry-run etc.
```

## Conventions

- Rust edition 2024, toolchain ≥ 1.96. `cargo fmt` + `cargo clippy -- -D warnings` must pass.
- Tests: unit per pipeline stage; shared contract-test suite run against every provider; `wiremock` for GitHub/GitLab (no network in tests); `insta` snapshots for E2E tree comparison; idempotency invariant (sync twice = same state). Windows is a first-class target (junctions vs symlinks).
- Commits: **no signing** (repo config `commit.gpgsign=false` is already set — don't override it). Never commit `docs/PLAN.md` / `docs/SPEC.md`.
- Delegate large implementation chunks to subagents to keep the main context lean.

## Commands

```
cargo build --workspace
cargo test --workspace
cargo run -p skills-cli -- update --dry-run
```
