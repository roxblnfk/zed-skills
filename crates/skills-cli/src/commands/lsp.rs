use crate::CliError;

/// Run the LSP server on stdio until the client disconnects (editors launch
/// this as a language server; see docs for the Zed integration).
pub async fn run() -> Result<(), CliError> {
    skills_lsp::run_stdio()
        .await
        .map_err(|e| CliError::config(format!("lsp server failed: {e}")))
}
