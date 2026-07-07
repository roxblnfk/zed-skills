//! `skills-lsp` — a diagnostics-first LSP server for `skills.json` and
//! `SKILL.md` (see docs/ZED_INTEGRATION.md for the architecture rationale).
//!
//! - `skills.json`: parse/validation errors with field-accurate spans, plus
//!   a read-only pipeline dry analysis (donor conflicts, unknown allowlist
//!   names, lockfile staleness, not-yet-fetched remotes) — cache-only, no
//!   network, no writes.
//! - `SKILL.md`: frontmatter + StaticAuditor findings over the buffer text,
//!   frontmatter validation (duplicate keys, spec length limits, name
//!   format, bool/enum values — `fm-*` codes), plus frontmatter completion
//!   (known keys, bool/enum values, a bootstrap `---` block snippet) and
//!   hover docs on known frontmatter keys.
//! - `skills.json` document links: `sources[]` packages → repo web URLs,
//!   by-url entries → the URL, dir-entry paths → the directory.
//! - Code action "Run skills update" → `workspace/executeCommand`
//!   `skills.update` runs the real pipeline in-process.
//! - Source code action "skills: set up gutter tasks" → `skills.setupTasks`
//!   generates/merges `.zed/tasks.json` entries running this very binary
//!   (Zed's ▶ runnables then bypass `skills` on PATH); startup reconciles
//!   stale binary paths after extension updates.
//! - Own `notify` FS watcher re-triggers analysis on external changes.

pub mod analysis;
pub mod completion;
pub mod fmcheck;
pub mod hover;
pub mod links;
pub mod offline;
pub mod server;
pub mod spanindex;
pub mod tasks;
pub mod update;
pub mod watch;

pub use server::{Backend, SETUP_TASKS_COMMAND, UPDATE_COMMAND};

/// Serve LSP over stdio until the client disconnects.
pub async fn run_stdio() -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = tower_lsp_server::LspService::new(Backend::new);
    tower_lsp_server::Server::new(stdin, stdout, socket)
        .serve(service)
        .await;
    Ok(())
}
