//! In-memory LSP test harness: serves the backend over a `tokio::io::duplex`
//! pipe and speaks framed JSON-RPC from the test side.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf};

use skills_lsp::Backend;
use tower_lsp_server::ls_types::Uri;
use tower_lsp_server::{LspService, Server};

/// How long a single expected message may take to arrive.
const RECV_TIMEOUT: Duration = Duration::from_secs(30);

pub struct TestClient {
    writer: WriteHalf<DuplexStream>,
    reader: ReadHalf<DuplexStream>,
    buffer: Vec<u8>,
    /// Messages received while waiting for something else.
    pending: VecDeque<Value>,
    next_id: i64,
}

impl TestClient {
    /// Spawn a fresh server and connect to it in-memory.
    pub fn start() -> TestClient {
        let (client_side, server_side) = tokio::io::duplex(1024 * 1024);
        let (server_read, server_write) = tokio::io::split(server_side);
        let (service, socket) = LspService::new(Backend::new);
        tokio::spawn(async move {
            Server::new(server_read, server_write, socket)
                .serve(service)
                .await;
        });
        let (reader, writer) = tokio::io::split(client_side);
        TestClient {
            writer,
            reader,
            buffer: Vec::new(),
            pending: VecDeque::new(),
            next_id: 0,
        }
    }

    /// `initialize` + `initialized` handshake with an optional workspace root.
    pub async fn initialize(&mut self, root: Option<&Path>) -> Value {
        let root_uri = root.map(uri_string);
        let params = json!({
            "capabilities": {},
            "rootUri": root_uri,
            "workspaceFolders": root_uri.as_ref().map(|uri| {
                vec![json!({ "uri": uri, "name": "test" })]
            }),
        });
        let result = self.request("initialize", params).await;
        self.notify("initialized", json!({})).await;
        result
    }

    pub async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await;
        let response = self
            .wait_for(|msg| msg.get("id").and_then(Value::as_i64) == Some(id))
            .await;
        if let Some(error) = response.get("error") {
            panic!("request {method} failed: {error}");
        }
        response.get("result").cloned().unwrap_or(Value::Null)
    }

    pub async fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await;
    }

    pub async fn did_open(&mut self, uri: &str, language: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": {
                "uri": uri, "languageId": language, "version": 1, "text": text,
            }}),
        )
        .await;
    }

    pub async fn did_change_full(&mut self, uri: &str, version: i64, text: &str) {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [ { "text": text } ],
            }),
        )
        .await;
    }

    /// Next `textDocument/publishDiagnostics` for `uri`.
    pub async fn wait_diagnostics(&mut self, uri: &str) -> Vec<Value> {
        let msg = self
            .wait_for(|msg| {
                msg.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
                    && msg
                        .pointer("/params/uri")
                        .and_then(Value::as_str)
                        .is_some_and(|u| uri_eq(u, uri))
            })
            .await;
        msg.pointer("/params/diagnostics")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    /// Wait for publishes until one satisfies `pred` (skips intermediate
    /// publishes for the same uri — watcher/update races may deliver an
    /// older state first).
    pub async fn wait_diagnostics_until(
        &mut self,
        uri: &str,
        pred: impl Fn(&[Value]) -> bool,
    ) -> Vec<Value> {
        loop {
            let diags = self.wait_diagnostics(uri).await;
            if pred(&diags) {
                return diags;
            }
        }
    }

    /// Next `window/showMessage` notification as `(type, message)`.
    pub async fn wait_show_message(&mut self) -> (i64, String) {
        let msg = self
            .wait_for(|msg| msg.get("method").and_then(Value::as_str) == Some("window/showMessage"))
            .await;
        (
            msg.pointer("/params/type")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            msg.pointer("/params/message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        )
    }

    /// Next `window/logMessage` notification as `(type, message)`.
    pub async fn wait_log_message(&mut self) -> (i64, String) {
        let msg = self
            .wait_for(|msg| msg.get("method").and_then(Value::as_str) == Some("window/logMessage"))
            .await;
        (
            msg.pointer("/params/type")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            msg.pointer("/params/message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        )
    }

    async fn wait_for(&mut self, pred: impl Fn(&Value) -> bool) -> Value {
        if let Some(pos) = self.pending.iter().position(&pred) {
            return self.pending.remove(pos).expect("position exists");
        }
        let deadline = tokio::time::Instant::now() + RECV_TIMEOUT;
        loop {
            let msg = tokio::time::timeout_at(deadline, self.read_message())
                .await
                .expect("timed out waiting for an LSP message");
            if pred(&msg) {
                return msg;
            }
            self.pending.push_back(msg);
        }
    }

    async fn send(&mut self, msg: Value) {
        let body = msg.to_string();
        let framed = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        self.writer
            .write_all(framed.as_bytes())
            .await
            .expect("write to server");
    }

    async fn read_message(&mut self) -> Value {
        loop {
            if let Some(msg) = self.try_parse() {
                return msg;
            }
            let mut chunk = [0u8; 4096];
            let n = self
                .reader
                .read(&mut chunk)
                .await
                .expect("read from server");
            assert!(n > 0, "server closed the stream");
            self.buffer.extend_from_slice(&chunk[..n]);
        }
    }

    fn try_parse(&mut self) -> Option<Value> {
        let header_end = self.buffer.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
        let headers = String::from_utf8_lossy(&self.buffer[..header_end]).to_string();
        let content_length: usize = headers
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length:"))
            .expect("Content-Length header")
            .trim()
            .parse()
            .expect("numeric Content-Length");
        if self.buffer.len() < header_end + content_length {
            return None;
        }
        let body: Vec<u8> = self
            .buffer
            .drain(..header_end + content_length)
            .skip(header_end)
            .collect();
        Some(serde_json::from_slice(&body).expect("valid JSON-RPC body"))
    }
}

/// `file://` URI string for a filesystem path.
pub fn uri_string(path: &Path) -> String {
    let uri = Uri::from_file_path(path).expect("absolute path");
    serde_json::to_value(&uri)
        .expect("uri serializes")
        .as_str()
        .expect("uri is a string")
        .to_string()
}

/// URIs may differ in drive-letter case / percent-encoding across layers.
fn uri_eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Reduce raw diagnostics to a stable, snapshot-friendly shape.
pub fn simplify(diags: &[Value]) -> Vec<Value> {
    diags
        .iter()
        .map(|d| {
            let range = format!(
                "{}:{}-{}:{}",
                d.pointer("/range/start/line")
                    .and_then(Value::as_u64)
                    .unwrap_or(99),
                d.pointer("/range/start/character")
                    .and_then(Value::as_u64)
                    .unwrap_or(99),
                d.pointer("/range/end/line")
                    .and_then(Value::as_u64)
                    .unwrap_or(99),
                d.pointer("/range/end/character")
                    .and_then(Value::as_u64)
                    .unwrap_or(99),
            );
            let severity = match d.get("severity").and_then(Value::as_u64) {
                Some(1) => "error",
                Some(2) => "warning",
                Some(3) => "info",
                Some(4) => "hint",
                _ => "none",
            };
            json!({
                "range": range,
                "severity": severity,
                "code": d.get("code").cloned().unwrap_or(Value::Null),
                "message": d.get("message").cloned().unwrap_or(Value::Null),
            })
        })
        .collect()
}

/// Write a file, creating parent dirs.
pub fn write_file(path: &Path, content: &str) {
    std::fs::create_dir_all(path.parent().expect("has parent")).expect("mkdir");
    std::fs::write(path, content).expect("write fixture");
}
