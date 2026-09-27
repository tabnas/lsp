// Copyright (c) 2026 Richard Rodger, MIT License

//! The protocol front-end (design §11): a thin JSON-RPC wiring over the
//! pipeline. Mirrors `ts/src/server.js` (canonical: capabilities,
//! incremental sync, the 150 ms debounce, version-stamped push
//! diagnostics, stale structural results suppressed rather than served,
//! `tabnas/status`, workspace grammars from `initializationOptions` and
//! `.tabnas/lsp.json`, hot reload through `didChangeWatchedFiles`) and
//! `go/server.go` (the closest model: a single-threaded message loop,
//! no async, document state needing no locking).
//!
//! The loop: messages arrive from the [`Connection`]'s reader thread;
//! each is handled in arrival order; a document change records a
//! deadline `debounce` from now, and the loop waits for the next message
//! only until the earliest deadline, then analyzes the documents that
//! are due. Diagnostics are pushed with the document's version at the
//! time of the parse; a document symbol, semantic tokens or hover
//! request is served from the analysis cached for the document's
//! CURRENT version and empty otherwise.
//!
//! What the Rust server adds to the TypeScript one, by the port brief:
//! the position encoding is negotiated (UTF-8 when the client offers it,
//! UTF-16 otherwise; [`Server::negotiate_encoding`]); documents larger
//! than `Config::max_document_bytes` are refused with a diagnostic; a
//! parse is stopped at `Config::parse_deadline` through the engine's
//! `parse_budget`.
//!
//! Status: [`Server::capabilities`] and [`Server::negotiate_encoding`]
//! are complete; the loop, the handlers, the manifest reader and
//! `serve` are signatures for the server module agent, with
//! `rs/tests/server_test.rs` (the scripted session of
//! `go/server_test.go` over the built binary) as the contract.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value};

use crate::analyze::Analysis;
use crate::completion::TRIGGER_CHARACTERS;
use crate::documents::DocumentStore;
use crate::instances::Instances;
use crate::jsonrpc::{Connection, Message, RpcError};
use crate::registry::Router;
use crate::semantic::LEGEND;
use crate::types::{Config, Entry, LoadError, PositionEncoding};

/// How long after the last change a document is re-analyzed
/// (`DEBOUNCE_MS` in `ts/src/server.js`).
pub const DEBOUNCE_MS: u64 = 150;

/// The per-workspace-folder grammar manifest (`WORKSPACE_MANIFEST`).
pub const WORKSPACE_MANIFEST: &str = ".tabnas/lsp.json";

/// The glob patterns the server asks the client to watch for hot reload:
/// the manifest, the dialect grammar files, and JSON (L2 specs).
pub const WATCHED_GLOBS: [&str; 3] = ["**/.tabnas/lsp.json", "**/*.{abnf,ebnf,gbnf}", "**/*.json"];

/// The running server (`Server` in `go/server.go`; the closure state of
/// `startServer` in TypeScript).
pub struct Server {
    config: Config,
    conn: Connection,
    router: Router,
    docs: DocumentStore,
    instances: Instances,
    /// The analysis cached per URI, with the document version it was
    /// made from.
    analyses: HashMap<String, (i64, Analysis)>,
    /// Documents due for analysis, by URI, with their deadlines.
    pending: HashMap<String, Instant>,
    encoding: PositionEncoding,
    workspace_folders: Vec<PathBuf>,
    /// `initializationOptions.languages`, kept to rebuild the workspace
    /// tier when a manifest changes.
    init_languages: Vec<Entry>,
    exited: bool,
}

impl Server {
    /// A server over a connection, not yet running.
    #[allow(unused_variables)] // stub
    pub fn new(config: Config, conn: Connection) -> Server {
        todo!("server::Server::new")
    }

    /// The message loop, until `exit` or the client closes the stream.
    /// Returns `Ok(())` on an orderly end.
    pub fn run(&mut self) -> Result<(), RpcError> {
        todo!("server::Server::run: the loop with debounce deadlines")
    }

    /// Dispatch one message.
    #[allow(unused_variables)] // stub
    pub fn handle(&mut self, message: Message) -> Result<(), RpcError> {
        todo!("server::Server::handle: go/server.go Handle")
    }

    /// The `initialize` result's `capabilities`: incremental sync,
    /// completion with the trigger characters, document symbols,
    /// semantic tokens (full, over the fixed legend), hover, and the
    /// negotiated position encoding.
    pub fn capabilities(encoding: PositionEncoding) -> Value {
        json!({
            "positionEncoding": encoding.as_str(),
            "textDocumentSync": { "openClose": true, "change": 2 },
            "completionProvider": { "triggerCharacters": TRIGGER_CHARACTERS },
            "documentSymbolProvider": true,
            "semanticTokensProvider": {
                "legend": {
                    "tokenTypes": LEGEND.iter().map(|kind| kind.name()).collect::<Vec<_>>(),
                    "tokenModifiers": Vec::<&str>::new(),
                },
                "full": true,
            },
            "hoverProvider": true,
        })
    }

    /// The encoding to answer a client with: UTF-8 when its
    /// `general.positionEncodings` offers it, else UTF-16 (the protocol
    /// default, and what a client that offers nothing gets).
    pub fn negotiate_encoding(client_capabilities: &Value) -> PositionEncoding {
        let offered = client_capabilities
            .get("general")
            .and_then(|general| general.get("positionEncodings"))
            .and_then(Value::as_array);
        match offered {
            Some(list)
                if list
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|name| name == PositionEncoding::Utf8.as_str()) =>
            {
                PositionEncoding::Utf8
            }
            _ => PositionEncoding::Utf16,
        }
    }

    /// The `tabnas/status` result: every entry's language id, enabled
    /// flag, source tier, quarantine state and lex stream.
    pub fn status(&self) -> Value {
        todo!("server::Server::status")
    }

    /// The negotiated encoding.
    pub fn encoding(&self) -> PositionEncoding {
        self.encoding
    }

    /// Whether `exit` was received.
    pub fn exited(&self) -> bool {
        self.exited
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn docs(&self) -> &DocumentStore {
        &self.docs
    }

    pub fn router(&self) -> &Router {
        &self.router
    }

    pub fn instances(&self) -> &Instances {
        &self.instances
    }

    /// The folders the client opened.
    pub fn workspace_folders(&self) -> &[PathBuf] {
        &self.workspace_folders
    }

    /// The documents awaiting analysis.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// `initializationOptions.languages`.
    pub fn init_languages(&self) -> &[Entry] {
        &self.init_languages
    }

    pub fn analyses(&self) -> usize {
        self.analyses.len()
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("config", &self.config)
            .field("docs", &self.docs.len())
            .field("analyses", &self.analyses.len())
            .field("pending", &self.pending.len())
            .field("encoding", &self.encoding)
            .field("workspace_folders", &self.workspace_folders)
            .field("exited", &self.exited)
            .finish_non_exhaustive()
    }
}

/// Run a server over standard input and output until the client is
/// done (`Serve` in Go; `startServer` in TypeScript).
#[allow(unused_variables)] // stub
pub fn serve(config: Config) -> Result<(), RpcError> {
    todo!("server::serve")
}

/// Read a folder's `.tabnas/lsp.json`: `{"languages": [entry, ...]}`,
/// each entry stamped workspace-sourced with the folder as its sandbox
/// dir. No manifest is the common case and yields no entries; a
/// malformed one is an error the server reports and never fatal.
#[allow(unused_variables)] // stub
pub fn read_workspace_manifest(folder: &Path) -> Result<Vec<Entry>, LoadError> {
    todo!("server::read_workspace_manifest")
}

/// A workspace folder or watched file as a path: a `file:` URI decoded,
/// a plain path as given, anything else `None`.
#[allow(unused_variables)] // stub
pub fn folder_path_of(uri_or_path: &str) -> Option<PathBuf> {
    todo!("server::folder_path_of")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_capabilities_advertise_the_pipeline() {
        let capabilities = Server::capabilities(PositionEncoding::Utf16);
        assert_eq!(capabilities["positionEncoding"], "utf-16");
        assert_eq!(capabilities["textDocumentSync"]["change"], 2);
        assert_eq!(capabilities["documentSymbolProvider"], true);
        assert_eq!(capabilities["hoverProvider"], true);
        assert_eq!(capabilities["semanticTokensProvider"]["full"], true);
        let legend = &capabilities["semanticTokensProvider"]["legend"]["tokenTypes"];
        assert_eq!(legend.as_array().map(Vec::len), Some(LEGEND.len()));
        assert_eq!(legend[0], "string");
        assert_eq!(
            capabilities["completionProvider"]["triggerCharacters"]
                .as_array()
                .map(Vec::len),
            Some(TRIGGER_CHARACTERS.len())
        );
        assert_eq!(
            Server::capabilities(PositionEncoding::Utf8)["positionEncoding"],
            "utf-8"
        );
    }

    #[test]
    fn utf8_is_negotiated_only_when_offered() {
        assert_eq!(
            Server::negotiate_encoding(&json!({})),
            PositionEncoding::Utf16
        );
        assert_eq!(
            Server::negotiate_encoding(&json!({"general": {"positionEncodings": ["utf-16"]}})),
            PositionEncoding::Utf16
        );
        assert_eq!(
            Server::negotiate_encoding(
                &json!({"general": {"positionEncodings": ["utf-32", "utf-8", "utf-16"]}})
            ),
            PositionEncoding::Utf8
        );
        assert_eq!(
            Server::negotiate_encoding(&json!({"general": {"positionEncodings": "utf-8"}})),
            PositionEncoding::Utf16,
            "a malformed offer is no offer"
        );
    }
}
