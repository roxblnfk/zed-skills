//! The `skills lsp` language server backend.
//!
//! Diagnostics-first (see docs/ZED_INTEGRATION.md): push diagnostics for
//! `skills.json` / `SKILL.md` buffers, a "Run skills update" code action on
//! staleness diagnostics, `workspace/executeCommand` `skills.update`, and an
//! own `notify` FS watcher for changes made outside the editor.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tower_lsp_server::jsonrpc::{Error, Result};
use tower_lsp_server::ls_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, CodeActionResponse, Command, Diagnostic,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, ExecuteCommandOptions, ExecuteCommandParams, InitializeParams,
    InitializeResult, InitializedParams, MessageType, NumberOrString, ServerCapabilities,
    ServerInfo, TextDocumentSyncCapability, TextDocumentSyncKind, Uri,
};
use tower_lsp_server::{Client, LanguageServer};

use skills_core::manifest::{MANIFEST_NAME, Manifest};

use crate::watch::WatchHandle;
use crate::{analysis, update, watch};

/// Debounce window for `didChange` bursts.
const CHANGE_DEBOUNCE: Duration = Duration::from_millis(300);

/// The `workspace/executeCommand` command id.
pub const UPDATE_COMMAND: &str = "skills.update";

/// Documents this server cares about, selected by basename.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocKind {
    /// `skills.json` — the project manifest.
    Manifest,
    /// `SKILL.md` — a skill body (usually under the target or a vendor dir).
    SkillMd,
}

struct Doc {
    kind: DocKind,
    /// Absolute filesystem path of the document.
    path: PathBuf,
    text: String,
    /// Bumped on every content change; analysis results carrying an older
    /// generation are dropped instead of published.
    generation: u64,
}

struct State {
    /// Workspace root from `initialize` (fallback project root).
    init_root: Mutex<Option<PathBuf>>,
    docs: Mutex<HashMap<Uri, Doc>>,
    /// Serializes `skills.update` executions; `try_lock` failure means one
    /// is already running.
    update_gate: tokio::sync::Mutex<()>,
    /// The active FS watcher (replaced when the watch set is re-resolved,
    /// dropped on shutdown).
    watcher: Mutex<Option<WatchHandle>>,
    watch_root: Mutex<Option<PathBuf>>,
}

#[derive(Clone)]
pub struct Backend {
    client: Client,
    state: Arc<State>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Backend {
            client,
            state: Arc::new(State {
                init_root: Mutex::new(None),
                docs: Mutex::new(HashMap::new()),
                update_gate: tokio::sync::Mutex::new(()),
                watcher: Mutex::new(None),
                watch_root: Mutex::new(None),
            }),
        }
    }

    fn classify(uri: &Uri) -> Option<(DocKind, PathBuf)> {
        let path = uri.to_file_path()?.into_owned();
        let name = path.file_name()?.to_str()?;
        let kind = if name.eq_ignore_ascii_case(MANIFEST_NAME) {
            DocKind::Manifest
        } else if name.eq_ignore_ascii_case("SKILL.md") {
            DocKind::SkillMd
        } else {
            return None;
        };
        Some((kind, path))
    }

    /// Project root for a document: the dir containing `skills.json`, walked
    /// up from the document when needed (SKILL.md lives under target/vendor
    /// dirs), falling back to the initialize root.
    fn project_root_for(&self, uri: &Uri) -> Option<PathBuf> {
        if let Some((kind, path)) = Self::classify(uri) {
            match kind {
                DocKind::Manifest => return path.parent().map(Path::to_path_buf),
                DocKind::SkillMd => {
                    let mut dir = path.parent();
                    while let Some(d) = dir {
                        if d.join(MANIFEST_NAME).is_file() {
                            return Some(d.to_path_buf());
                        }
                        dir = d.parent();
                    }
                }
            }
        }
        self.state.init_root.lock().expect("state lock").clone()
    }

    /// Recompute and publish diagnostics for one open document, unless its
    /// generation moved on (rapid edits) — then the newer schedule wins.
    fn schedule_analysis(&self, uri: Uri, generation: u64, debounce: bool) {
        let backend = self.clone();
        tokio::spawn(async move {
            if debounce {
                tokio::time::sleep(CHANGE_DEBOUNCE).await;
            }
            let snapshot = {
                let docs = backend.state.docs.lock().expect("state lock");
                docs.get(&uri).and_then(|doc| {
                    (doc.generation == generation)
                        .then(|| (doc.kind, doc.path.clone(), doc.text.clone()))
                })
            };
            let Some((kind, path, text)) = snapshot else {
                return; // closed or superseded
            };
            let diagnostics = backend.compute_diagnostics(kind, &path, &text).await;
            let current = {
                let docs = backend.state.docs.lock().expect("state lock");
                docs.get(&uri).map(|doc| doc.generation)
            };
            if current != Some(generation) {
                return; // stale result — a newer edit landed during analysis
            }
            backend
                .client
                .publish_diagnostics(uri, diagnostics, None)
                .await;
        });
    }

    async fn compute_diagnostics(&self, kind: DocKind, path: &Path, text: &str) -> Vec<Diagnostic> {
        match kind {
            DocKind::Manifest => {
                let root = path.parent().unwrap_or(Path::new("."));
                analysis::analyze_manifest(root, text).await
            }
            DocKind::SkillMd => {
                let dir_name = path
                    .parent()
                    .and_then(|d| d.file_name())
                    .and_then(|n| n.to_str())
                    .map(str::to_string);
                analysis::analyze_skill_md(text, dir_name.as_deref())
            }
        }
    }

    /// Re-analyze every open document (watcher events, post-update refresh).
    async fn reanalyze_open_docs(&self) {
        let scheduled: Vec<(Uri, u64)> = {
            let docs = self.state.docs.lock().expect("state lock");
            docs.iter()
                .map(|(uri, doc)| (uri.clone(), doc.generation))
                .collect()
        };
        for (uri, generation) in scheduled {
            self.schedule_analysis(uri, generation, false);
        }
    }

    /// (Re)start the FS watcher rooted at `root`. Replacing the handle stops
    /// the previous watcher thread and ends its event loop task.
    fn start_watcher(&self, root: PathBuf) {
        let manifest = Manifest::load(&root.join(MANIFEST_NAME)).ok();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let Ok(handle) = watch::start(&root, manifest.as_ref(), tx) else {
            return; // root vanished — nothing to watch
        };
        {
            let mut watcher = self.state.watcher.lock().expect("state lock");
            *watcher = Some(handle);
        }
        {
            let mut watch_root = self.state.watch_root.lock().expect("state lock");
            *watch_root = Some(root.clone());
        }
        let backend = self.clone();
        tokio::spawn(async move {
            // Ends when the watcher handle is dropped (shutdown/replacement):
            // the sender inside the debouncer closure goes away with it.
            while let Some(manifest_changed) = rx.recv().await {
                backend.reanalyze_open_docs().await;
                if manifest_changed {
                    // The watch set derives from the manifest — re-resolve.
                    // This replaces the watcher; the loop ends with its channel.
                    backend.start_watcher(root.clone());
                    break;
                }
            }
        });
    }

    /// Start the watcher for a manifest document's project if none is
    /// running for that root yet.
    fn ensure_watcher(&self, root: &Path) {
        let already = {
            let watch_root = self.state.watch_root.lock().expect("state lock");
            watch_root.as_deref() == Some(root)
        };
        if !already {
            self.start_watcher(root.to_path_buf());
        }
    }

    async fn run_update_command(&self, root: &Path) {
        let Ok(_guard) = self.state.update_gate.try_lock() else {
            self.client
                .show_message(
                    MessageType::WARNING,
                    "skills update is already running — please wait for it to finish",
                )
                .await;
            return;
        };
        match update::run_real_update(root).await {
            Ok(report) => {
                self.client
                    .show_message(MessageType::INFO, update::summarize(&report))
                    .await;
            }
            Err(message) => {
                self.client
                    .show_message(
                        MessageType::ERROR,
                        format!("skills update failed: {message}"),
                    )
                    .await;
            }
        }
        // Post-update state changed on disk — refresh all diagnostics.
        self.reanalyze_open_docs().await;
    }
}

/// Diagnostic codes whose fix is running `skills update`.
fn actionable(diagnostic: &Diagnostic) -> bool {
    matches!(
        &diagnostic.code,
        Some(NumberOrString::String(code))
            if code == analysis::codes::STALE || code == analysis::codes::NOT_FETCHED
    )
}

impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        let root = params
            .workspace_folders
            .as_ref()
            .and_then(|folders| folders.first())
            .and_then(|folder| folder.uri.to_file_path())
            .map(|p| p.into_owned())
            .or_else(|| {
                #[allow(deprecated)]
                params
                    .root_uri
                    .as_ref()
                    .and_then(|uri| uri.to_file_path())
                    .map(|p| p.into_owned())
            });
        *self.state.init_root.lock().expect("state lock") = root;

        Ok(InitializeResult {
            offset_encoding: None,
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec![UPDATE_COMMAND.to_string()],
                    ..ExecuteCommandOptions::default()
                }),
                ..ServerCapabilities::default()
            },
            server_info: Some(ServerInfo {
                name: "skills-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _params: InitializedParams) {
        let root = self.state.init_root.lock().expect("state lock").clone();
        if let Some(root) = root {
            self.ensure_watcher(&root);
        }
    }

    async fn shutdown(&self) -> Result<()> {
        // Stop the watcher thread; its event-loop task ends with the channel.
        self.state.watcher.lock().expect("state lock").take();
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let Some((kind, path)) = Self::classify(&uri) else {
            return;
        };
        if kind == DocKind::Manifest
            && let Some(root) = path.parent()
        {
            self.ensure_watcher(root);
        }
        let generation = {
            let mut docs = self.state.docs.lock().expect("state lock");
            docs.insert(
                uri.clone(),
                Doc {
                    kind,
                    path,
                    text: params.text_document.text,
                    generation: 0,
                },
            );
            0
        };
        self.schedule_analysis(uri, generation, false);
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        // Full sync: the last change carries the complete new text.
        let Some(change) = params.content_changes.into_iter().next_back() else {
            return;
        };
        let generation = {
            let mut docs = self.state.docs.lock().expect("state lock");
            let Some(doc) = docs.get_mut(&uri) else {
                return;
            };
            doc.text = change.text;
            doc.generation += 1;
            doc.generation
        };
        self.schedule_analysis(uri, generation, true);
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        let scheduled = {
            let docs = self.state.docs.lock().expect("state lock");
            docs.get(&uri).map(|doc| (doc.kind, doc.generation))
        };
        let Some((kind, generation)) = scheduled else {
            return;
        };
        if kind == DocKind::Manifest {
            // The manifest on disk changed — the watch set may be stale.
            if let Some(root) = self.project_root_for(&uri) {
                self.start_watcher(root);
            }
        }
        self.schedule_analysis(uri, generation, false);
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        let removed = {
            let mut docs = self.state.docs.lock().expect("state lock");
            docs.remove(&uri).is_some()
        };
        if removed {
            // We are the only diagnostics source for these files.
            self.client.publish_diagnostics(uri, Vec::new(), None).await;
        }
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let matching: Vec<Diagnostic> = params
            .context
            .diagnostics
            .iter()
            .filter(|d| actionable(d))
            .cloned()
            .collect();
        if matching.is_empty() {
            return Ok(None);
        }
        let Some(root) = self.project_root_for(&params.text_document.uri) else {
            return Ok(None);
        };
        let action = CodeAction {
            title: "Run skills update".to_string(),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(matching),
            command: Some(Command {
                title: "Run skills update".to_string(),
                command: UPDATE_COMMAND.to_string(),
                arguments: Some(vec![serde_json::Value::String(
                    root.to_string_lossy().into_owned(),
                )]),
            }),
            ..CodeAction::default()
        };
        Ok(Some(vec![CodeActionOrCommand::CodeAction(action)]))
    }

    async fn execute_command(
        &self,
        params: ExecuteCommandParams,
    ) -> Result<Option<serde_json::Value>> {
        if params.command != UPDATE_COMMAND {
            return Err(Error::invalid_params(format!(
                "unknown command '{}'",
                params.command
            )));
        }
        let root = params
            .arguments
            .first()
            .and_then(|arg| arg.as_str())
            .map(PathBuf::from)
            .or_else(|| self.state.init_root.lock().expect("state lock").clone());
        let Some(root) = root else {
            return Err(Error::invalid_params(
                "skills.update: no project root (pass it as the first argument)",
            ));
        };
        self.run_update_command(&root).await;
        Ok(None)
    }
}
