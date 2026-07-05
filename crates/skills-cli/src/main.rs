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
        /// Run the full pipeline (including conflict detection) without
        /// writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Override the sync target from skills.json.
        #[arg(long, value_name = "PATH")]
        target: Option<String>,
    },
    /// List donors and skills with their sync status. Read-only.
    Show,
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
}

impl From<PipelineError> for CliError {
    fn from(err: PipelineError) -> Self {
        let code = match &err {
            PipelineError::Prepare(_) | PipelineError::Sync(_) => 1,
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
        Command::Update { dry_run, target } => commands::update::run(&cwd, dry_run, target).await,
        Command::Show => commands::show::run(&cwd).await,
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {}", err.message);
            ExitCode::from(err.code)
        }
    }
}
