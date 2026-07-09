//! `skills` — CLI skills manager for AI coding agents.

mod commands;
mod render;

use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{Parser, Subcommand};

use skills_core::error::PipelineError;

#[derive(Parser)]
#[command(
    name = "skills",
    version,
    about = "Sync AI skills from vendors into a project-local directory"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Bootstrap a stub skills.json in the current directory.
    Init {
        /// Overwrite an existing skills.json.
        #[arg(long)]
        force: bool,
    },
    /// Sync skills from all configured donors into the target directory.
    #[command(after_help = "Exit codes:\n  \
        0  success / in sync\n  \
        1  usage or config error\n  \
        2  skill name conflict\n  \
        3  audit block\n  \
        4  network or provider error\n  \
        5  changes pending (--check only)")]
    Update {
        /// Restrict the sync to matching packages (`vendor/package` or
        /// `vendor/*`). Naming a package filters to it and implicitly trusts
        /// it.
        #[arg(value_name = "PACKAGE")]
        packages: Vec<String>,
        /// Run the full pipeline (including conflict detection) without
        /// writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Check whether the target is in sync with the donors without
        /// writing anything (compact output; normal network/cache semantics,
        /// so remote drift is detected). Exit 0 when in sync, 5 when changes
        /// are pending.
        #[arg(long, conflicts_with = "dry_run")]
        check: bool,
        /// Override the sync target from skills.json.
        #[arg(long, value_name = "PATH")]
        target: Option<String>,
        /// Extra path to mirror the target into as a junction (Windows) or
        /// symlink (POSIX). Repeatable; passing it at all replaces the
        /// project `aliases` entirely (no merge).
        #[arg(long = "alias", value_name = "PATH")]
        alias: Vec<String>,
        /// Only run the named provider (dir, composer, github, gitlab, url).
        #[arg(long, value_name = "ID")]
        from: Option<String>,
        /// Delete matching cache entries and re-download remote archives
        /// (does not bypass the audit verdict cache; see --re-audit).
        #[arg(long)]
        refresh: bool,
        /// Bypass the lockfile audit-verdict cache and re-run the audit
        /// chain for every skill.
        #[arg(long)]
        re_audit: bool,
        /// Extra trusted vendor pattern on top of the built-in and project
        /// lists (repeatable).
        #[arg(long = "trust", value_name = "PATTERN")]
        trust: Vec<String>,
    },
    /// List donors and skills with their sync status. Read-only.
    Show {
        /// Only show matching packages; non-matching donors are listed as
        /// filtered-out.
        #[arg(value_name = "PACKAGE")]
        packages: Vec<String>,
        /// Only show donors of the named provider (dir, composer, github,
        /// gitlab, url).
        #[arg(long, value_name = "ID")]
        from: Option<String>,
        /// Extra trusted vendor pattern (repeatable).
        #[arg(long = "trust", value_name = "PATTERN")]
        trust: Vec<String>,
    },
    /// Run the language server over stdio (diagnostics for skills.json and
    /// SKILL.md; editors launch this, not humans).
    Lsp,
    /// Register a remote donor in skills.json and sync it immediately.
    Add {
        /// `github:owner/repo`, `gitlab:group/project` or a repository URL
        /// (https / ssh / scp form).
        input: String,
        /// Exact ref (tag, branch, SHA) or caret constraint (`^1.2`).
        #[arg(long, value_name = "REF")]
        r#ref: Option<String>,
        /// Allowlist of skill (canonical) names to sync; repeatable.
        #[arg(long = "skill", value_name = "NAME")]
        skills: Vec<String>,
        /// Self-hosted GitHub/GitLab host (e.g. gitlab.example.com).
        #[arg(long, value_name = "HOST")]
        host: Option<String>,
    },
}

/// A command failure carrying its exit code (0 ok, 1 usage/config,
/// 2 conflict, 3 audit block, 4 provider error, 5 changes pending — the
/// `update --check` "out of sync" status, not a failure).
pub(crate) struct CliError {
    pub code: u8,
    /// Printed to stderr as `error: ...`; empty for pure status codes
    /// (exit 5) whose report already went to stdout.
    pub message: String,
}

impl CliError {
    fn config(message: impl Into<String>) -> Self {
        CliError {
            code: 1,
            message: message.into(),
        }
    }

    /// `update --check` found pending changes (exit code 5). The compact
    /// report is already on stdout; nothing is printed to stderr.
    fn changes_pending() -> Self {
        CliError {
            code: 5,
            message: String::new(),
        }
    }

    /// Network / provider failure (exit code 4).
    fn provider(message: impl Into<String>) -> Self {
        CliError {
            code: 4,
            message: message.into(),
        }
    }
}

impl From<PipelineError> for CliError {
    fn from(err: PipelineError) -> Self {
        let code = match &err {
            PipelineError::Prepare(_) | PipelineError::Trust(_) | PipelineError::Sync(_) => 1,
            PipelineError::Resolve(_) => 2,
            PipelineError::Audit(_) => 3,
            PipelineError::Discover(_) | PipelineError::Materialize(_) | PipelineError::Scan(_) => {
                4
            }
        };
        CliError {
            code,
            message: err.to_string(),
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err)
            if matches!(
                err.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            let _ = err.print();
            return ExitCode::SUCCESS;
        }
        Err(err) => {
            let _ = err.print();
            return ExitCode::from(1);
        }
    };

    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(e) => {
            eprintln!("error: cannot determine current directory: {e}");
            return ExitCode::from(1);
        }
    };

    let result = match cli.command {
        Command::Init { force } => commands::init::run(&cwd, force),
        Command::Update {
            packages,
            dry_run,
            check,
            target,
            alias,
            from,
            refresh,
            re_audit,
            trust,
        } => {
            commands::update::run(
                &cwd,
                dry_run,
                check,
                target,
                alias,
                from,
                refresh,
                re_audit,
                commands::RawFilters { packages, trust },
            )
            .await
        }
        Command::Show {
            packages,
            from,
            trust,
        } => commands::show::run(&cwd, from, commands::RawFilters { packages, trust }).await,
        Command::Lsp => commands::lsp::run().await,
        Command::Add {
            input,
            r#ref,
            skills,
            host,
        } => commands::add::run(&cwd, &input, r#ref, skills, host).await,
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            if !err.message.is_empty() {
                eprintln!("error: {}", err.message);
            }
            ExitCode::from(err.code)
        }
    }
}
