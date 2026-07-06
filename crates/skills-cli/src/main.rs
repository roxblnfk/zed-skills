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
    Update {
        /// Restrict the sync to matching packages (`vendor/package` or
        /// `vendor/*`). Naming a package implicitly trusts it and enables
        /// discovery for it.
        #[arg(value_name = "PACKAGE")]
        packages: Vec<String>,
        /// Run the full pipeline (including conflict detection) without
        /// writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Override the sync target from skills.json.
        #[arg(long, value_name = "PATH")]
        target: Option<String>,
        /// Only run the named provider (dir, composer, github, gitlab, url).
        #[arg(long, value_name = "ID")]
        from: Option<String>,
        /// Delete matching cache entries and re-download remote archives.
        #[arg(long)]
        refresh: bool,
        /// Extra trusted vendor pattern on top of the built-in and project
        /// lists (repeatable).
        #[arg(long = "trust", value_name = "PATTERN")]
        trust: Vec<String>,
        /// Include undeclared skills of trusted packages (well-known
        /// containers + bounded recursive fallback).
        #[arg(long)]
        discovery: bool,
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
        /// Include undeclared skills of trusted packages.
        #[arg(long)]
        discovery: bool,
    },
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

/// A command failure carrying its exit code (spec §10: 0 ok, 1 usage/config,
/// 2 conflict, 3 audit block, 4 provider error).
pub(crate) struct CliError {
    pub code: u8,
    pub message: String,
}

impl CliError {
    fn config(message: impl Into<String>) -> Self {
        CliError {
            code: 1,
            message: message.into(),
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
            target,
            from,
            refresh,
            trust,
            discovery,
        } => {
            commands::update::run(
                &cwd,
                dry_run,
                target,
                from,
                refresh,
                commands::RawFilters {
                    packages,
                    trust,
                    discovery,
                },
            )
            .await
        }
        Command::Show {
            packages,
            from,
            trust,
            discovery,
        } => {
            commands::show::run(
                &cwd,
                from,
                commands::RawFilters {
                    packages,
                    trust,
                    discovery,
                },
            )
            .await
        }
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
            eprintln!("error: {}", err.message);
            ExitCode::from(err.code)
        }
    }
}
