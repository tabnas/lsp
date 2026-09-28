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
//! Where this server departs from the TypeScript one, and why:
//!
//! - It logs through `window/logMessage`, as `connection.console` does,
//!   and answers malformed input with a JSON-RPC error rather than
//!   throwing: a body that is not JSON gets `ParseError`, params of the
//!   wrong shape get `InvalidParams` for a request and a log line for a
//!   notification.
//! - A grammar's own code failing is a panic the Rust engine catches
//!   and reports as an `internal` error that ends the parse. That is
//!   the canonical `analyze` THROWING (a grammar raising something other
//!   than a `TabnasError`), so the server does what the canonical one
//!   does with a throw: counts it toward the grammar's quarantine, logs
//!   it and publishes nothing. A panic that escapes the pipeline itself
//!   is treated the same way.
//! - The workspace-module trust gate is applied here, around the host's
//!   `MakeInstance`, because the callback is opaque: `initialize` can set
//!   `trustWorkspaceModules` after the host built its loader, where the
//!   TypeScript server mutates the loader's options object instead. A
//!   workspace entry whose load is a module is refused, with the
//!   canonical message, unless [`Config::trust_workspace_modules`] or
//!   the initialization option says otherwise. A host whose loader keeps
//!   its own gate (the binary's [`crate::Loader`]) hands the server that
//!   gate's flag through [`Server::with_trust`], so one setting opens
//!   both.
//! - A handler that panics does not end the session: a request gets
//!   `InternalError`, a notification a log line, which is what
//!   `vscode-jsonrpc` does with a handler that throws.
//! - `$/cancelRequest` is ignored: messages are handled in arrival order
//!   on one thread, so by the time a cancellation is read the request it
//!   names has been answered.
//! - After `exit` the loop ends; the binary exits 0 when `shutdown`
//!   came first and 1 otherwise, the protocol's rule, and the same when
//!   the client closes the stream without `exit`, as
//!   `vscode-languageserver` does.

use std::collections::{HashMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::analyze::{analyze, Analysis};
use crate::completion::{completion, TRIGGER_CHARACTERS};
use crate::documents::DocumentStore;
use crate::hover::hover;
use crate::instances::Instances;
use crate::jsonrpc::{
    Connection, Message, Received, RpcError, INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST,
    METHOD_NOT_FOUND,
};
use crate::registry::Router;
use crate::semantic::LEGEND;
use crate::types::{
    Config, Diagnostic, Entry, Load, LoadError, MakeInstance, Position, PositionEncoding, Range,
    SEVERITY_ERROR,
};

/// How long after the last change a document is re-analyzed
/// (`DEBOUNCE_MS` in `ts/src/server.js`).
pub const DEBOUNCE_MS: u64 = 150;

/// The per-workspace-folder grammar manifest (`WORKSPACE_MANIFEST`).
pub const WORKSPACE_MANIFEST: &str = ".tabnas/lsp.json";

/// The glob patterns the server asks the client to watch for hot reload:
/// the manifest, the dialect grammar files, and JSON (L2 specs).
pub const WATCHED_GLOBS: [&str; 3] = ["**/.tabnas/lsp.json", "**/*.{abnf,ebnf,gbnf}", "**/*.json"];

/// The id of the one dynamic registration the server makes: the
/// watched-files registration of hot reload.
pub const WATCH_REGISTRATION_ID: &str = "tabnas-lsp/watched-files";

/// How many engine steps pass between two looks at the parse deadline.
const DEADLINE_CHECK_EVERY: usize = 16;

/// `MessageType.Error` and `MessageType.Warning` for `window/logMessage`.
const LOG_ERROR: u32 = 1;
const LOG_WARNING: u32 = 2;

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
    /// The documents whose last published diagnostics may be non-empty,
    /// to be cleared when a document stops being served (its route
    /// removed by a manifest change) or closes.
    published: HashSet<String>,
    encoding: PositionEncoding,
    workspace_folders: Vec<PathBuf>,
    /// `initializationOptions.languages`, stamped, kept to rebuild the
    /// workspace tier when a manifest changes.
    init_languages: Vec<Entry>,
    /// The workspace-module trust gate the wrapped `MakeInstance` reads.
    trust: Arc<AtomicBool>,
    /// The deadline of the parse in progress, read by the budget check
    /// installed on every instance when `Config::parse_deadline` is set.
    deadline: Arc<Mutex<Option<Instant>>>,
    /// The next id for a request this server sends the client.
    next_id: i64,
    shutdown: bool,
    exited: bool,
}

impl Server {
    /// A server over a connection, not yet running. The router starts
    /// with the configured tiers; `initialize` adds the workspace tier.
    pub fn new(config: Config, conn: Connection) -> Server {
        let trust = Arc::new(AtomicBool::new(false));
        Server::with_trust(config, conn, trust)
    }

    /// [`Server::new`] with the workspace-module trust flag shared with
    /// the host: the server sets it when `initialize` grants trust (or
    /// [`Config::trust_workspace_modules`] does), and reads it at every
    /// make, so a host loader holding the same flag
    /// ([`crate::Loader::trust_handle`]) opens its own gate with the
    /// server's. The binary serves through this.
    pub fn with_trust(config: Config, conn: Connection, trust: Arc<AtomicBool>) -> Server {
        if config.trust_workspace_modules {
            trust.store(true, Ordering::SeqCst);
        }
        let deadline = Arc::new(Mutex::new(None));
        let make = gated_make_instance(
            Arc::clone(&config.make_instance),
            Arc::clone(&trust),
            config.parse_deadline.map(|_| Arc::clone(&deadline)),
        );
        let router = Router::new(
            config.entries.clone(),
            config.workspace_entries.clone(),
            config.user_entries.clone(),
        );
        Server {
            conn,
            router,
            docs: DocumentStore::new(),
            instances: Instances::new(make),
            analyses: HashMap::new(),
            pending: HashMap::new(),
            published: HashSet::new(),
            encoding: PositionEncoding::Utf16,
            workspace_folders: Vec::new(),
            init_languages: Vec::new(),
            trust,
            deadline,
            next_id: 1,
            shutdown: false,
            exited: false,
            config,
        }
    }

    /// The message loop, until `exit` or the client closes the stream.
    /// Returns `Ok(())` on an orderly end, and the error when the stream
    /// fails or a write does.
    ///
    /// Each turn first analyzes the documents whose debounce has run
    /// out, then waits for the next message no longer than the earliest
    /// remaining deadline.
    pub fn run(&mut self) -> Result<(), RpcError> {
        while !self.exited {
            self.analyze_due(Instant::now())?;
            let received = match self.next_deadline() {
                Some(at) => self
                    .conn
                    .recv_timeout(at.saturating_duration_since(Instant::now())),
                None => self.conn.recv(),
            };
            match received {
                Ok(Received::Message(message)) => self.handle_contained(message)?,
                Ok(Received::Timeout) => {}
                Ok(Received::Closed) => return Ok(()),
                Err(error) if error.is_recoverable() => self.reject(&error)?,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// [`Server::handle`], with a panic in a handler contained: a request
    /// is answered `InternalError` and a notification logged, in the
    /// words `vscode-jsonrpc` uses for a handler that throws, and the
    /// loop goes on. The pipeline catches a grammar's own panics before
    /// this; what reaches here is a defect in the server, and one bad
    /// message must not end the session.
    fn handle_contained(&mut self, message: Message) -> Result<(), RpcError> {
        let id = message.id.clone();
        let method = message.method.clone().unwrap_or_default();
        match catch_unwind(AssertUnwindSafe(|| self.handle(message))) {
            Ok(result) => result,
            Err(panic) => {
                let why = panic_message(panic.as_ref()).to_string();
                match id {
                    Some(id) => self.conn.respond_error(
                        id,
                        INTERNAL_ERROR,
                        format!("Request {method} failed with message: {why}"),
                    ),
                    None => {
                        log(
                            &self.conn,
                            LOG_ERROR,
                            &format!("Notification handler '{method}' failed with message: {why}"),
                        );
                        Ok(())
                    }
                }
            }
        }
    }

    /// Answer or log input that could not be read as a message.
    fn reject(&mut self, error: &RpcError) -> Result<(), RpcError> {
        match error.response() {
            Some(response) => self.conn.send(&response),
            None => {
                log(&self.conn, LOG_ERROR, &format!("tabnas-lsp: {error}"));
                Ok(())
            }
        }
    }

    /// Dispatch one message. A request is always answered, with a result
    /// or an error; a notification never is; a response from the client
    /// (to the watcher registration) is ignored. `Err` only when writing
    /// to the client fails.
    pub fn handle(&mut self, message: Message) -> Result<(), RpcError> {
        let Some(method) = message.method.clone() else {
            if message.id.is_some() && !message.is_response() {
                // An id and nothing else: not a request, not a response.
                let id = message.id.unwrap_or(Value::Null);
                return self.conn.respond_error(
                    id,
                    INVALID_REQUEST,
                    "Invalid request: a message needs a method, a result or an error",
                );
            }
            return Ok(());
        };
        let id = message.id;
        let params = message.params.unwrap_or(Value::Null);
        match method.as_str() {
            "initialize" => {
                let result = self.initialize(&params);
                self.answer(id, result)
            }
            "initialized" => self.initialized(),
            "shutdown" => {
                self.shutdown = true;
                self.answer(id, Value::Null)
            }
            "exit" => {
                self.exited = true;
                Ok(())
            }
            "textDocument/didOpen" => match decode::<DidOpen>(&params) {
                Ok(p) => {
                    let d = p.text_document;
                    self.docs
                        .open(d.uri.clone(), d.language_id, d.version, d.text);
                    self.schedule(&d.uri);
                    Ok(())
                }
                Err(error) => self.bad_params(id, &method, &error),
            },
            "textDocument/didChange" => match decode::<DidChange>(&params) {
                Ok(p) => {
                    self.did_change(p);
                    Ok(())
                }
                Err(error) => self.bad_params(id, &method, &error),
            },
            "textDocument/didClose" => match decode::<TextDocumentParams>(&params) {
                Ok(p) => {
                    let uri = p.text_document.uri;
                    self.docs.close(&uri);
                    self.analyses.remove(&uri);
                    self.pending.remove(&uri);
                    self.published.remove(&uri);
                    self.conn.notify(
                        "textDocument/publishDiagnostics",
                        json!({ "uri": uri, "diagnostics": [] }),
                    )
                }
                Err(error) => self.bad_params(id, &method, &error),
            },
            "workspace/didChangeWatchedFiles" => match decode::<DidChangeWatchedFiles>(&params) {
                Ok(p) => {
                    self.did_change_watched_files(&p);
                    Ok(())
                }
                Err(error) => self.bad_params(id, &method, &error),
            },
            "textDocument/completion" => match decode::<PositionParams>(&params) {
                Ok(p) => {
                    let items = self.completion(&p);
                    self.answer(id, items)
                }
                Err(error) => self.bad_params(id, &method, &error),
            },
            "textDocument/documentSymbol" => match decode::<TextDocumentParams>(&params) {
                Ok(p) => {
                    let symbols = match self.current_analysis(&p.text_document.uri) {
                        Some(analysis) => to_value(&analysis.outline),
                        None => json!([]),
                    };
                    self.answer(id, symbols)
                }
                Err(error) => self.bad_params(id, &method, &error),
            },
            "textDocument/semanticTokens/full" => match decode::<TextDocumentParams>(&params) {
                Ok(p) => {
                    let data = self
                        .current_analysis(&p.text_document.uri)
                        .and_then(|analysis| analysis.semantic_tokens.as_ref())
                        .map_or_else(|| json!([]), |tokens| to_value(&tokens.data));
                    self.answer(id, json!({ "data": data }))
                }
                Err(error) => self.bad_params(id, &method, &error),
            },
            "textDocument/hover" => match decode::<PositionParams>(&params) {
                Ok(p) => {
                    let result = self.hover(&p);
                    self.answer(id, result)
                }
                Err(error) => self.bad_params(id, &method, &error),
            },
            "tabnas/status" => {
                let status = self.status();
                self.answer(id, status)
            }
            _ => match id {
                Some(id) => self.conn.respond_error(
                    id,
                    METHOD_NOT_FOUND,
                    format!("Unhandled method {method}"),
                ),
                // Unknown notifications ($/cancelRequest, $/setTrace,
                // workspace/didChangeConfiguration, ...) are ignored, per
                // the protocol.
                None => Ok(()),
            },
        }
    }

    /// Answer a request; a notification (no id) gets nothing.
    fn answer(&self, id: Option<Value>, result: Value) -> Result<(), RpcError> {
        match id {
            Some(id) => self.conn.respond(id, result),
            None => Ok(()),
        }
    }

    /// Params that do not have the method's shape: `InvalidParams` for a
    /// request, a log line for a notification.
    fn bad_params(
        &self,
        id: Option<Value>,
        method: &str,
        error: &serde_json::Error,
    ) -> Result<(), RpcError> {
        let message = format!("invalid params for {method}: {error}");
        match id {
            Some(id) => self.conn.respond_error(id, INVALID_PARAMS, message),
            None => {
                log(&self.conn, LOG_ERROR, &format!("tabnas-lsp: {message}"));
                Ok(())
            }
        }
    }

    /// `onInitialize`: the trust option, the workspace folders (from
    /// `workspaceFolders`, else `rootUri`), the client's languages, the
    /// negotiated encoding, the workspace tier; then the capabilities.
    fn initialize(&mut self, params: &Value) -> Value {
        let init = params
            .get("initializationOptions")
            .filter(|init| init.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        if init.get("trustWorkspaceModules") == Some(&Value::Bool(true)) {
            self.trust.store(true, Ordering::SeqCst);
        }

        self.workspace_folders = params
            .get("workspaceFolders")
            .and_then(Value::as_array)
            .map(|folders| {
                folders
                    .iter()
                    .filter_map(|folder| folder.get("uri").and_then(Value::as_str))
                    .filter_map(folder_path_of)
                    .collect()
            })
            .unwrap_or_default();
        if self.workspace_folders.is_empty() {
            if let Some(root) = params
                .get("rootUri")
                .and_then(Value::as_str)
                .and_then(folder_path_of)
            {
                self.workspace_folders = vec![root];
            }
        }

        // Client-supplied entries are session-wide: `_dir` gives their
        // relative grammar paths a sandbox base, but `_scope` is null so
        // they route in EVERY folder (and for non-file documents).
        let default_dir = self
            .workspace_folders
            .first()
            .cloned()
            .or_else(|| std::env::current_dir().ok());
        let languages = init
            .get("languages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        self.init_languages = Vec::new();
        for (i, language) in languages.iter().enumerate() {
            match init_language_entry(language, default_dir.as_deref()) {
                Ok(entry) => self.init_languages.push(entry),
                Err(error) => log(
                    &self.conn,
                    LOG_WARNING,
                    &format!("tabnas-lsp: initializationOptions.languages[{i}] ignored: {error}"),
                ),
            }
        }

        self.encoding =
            Self::negotiate_encoding(params.get("capabilities").unwrap_or(&Value::Null));
        self.docs.set_encoding(self.encoding);
        self.rebuild_registry();

        json!({
            "capabilities": Self::capabilities(self.encoding),
            "serverInfo": { "name": self.config.server_name, "version": crate::VERSION },
        })
    }

    /// `onInitialized`: ask the client to watch the workspace grammar
    /// sources, so the L4 dev loop works (an edited spec or grammar file
    /// rebuilds its instance, an edited manifest rebuilds the tier).
    /// Whenever there is a workspace folder, not only when an entry
    /// already names a grammar file: the manifest watcher is part of the
    /// same registration, and a first manifest is when it matters most.
    /// Best effort, as in TypeScript: the client's answer is ignored, and
    /// a client without dynamic registration gets no hot reload.
    fn initialized(&mut self) -> Result<(), RpcError> {
        if self.workspace_folders.is_empty() {
            return Ok(());
        }
        let watchers: Vec<Value> = WATCHED_GLOBS
            .iter()
            .map(|glob| json!({ "globPattern": glob }))
            .collect();
        let id = self.next_id;
        self.next_id += 1;
        self.conn.send(&Message::request(
            json!(id),
            "client/registerCapability",
            Some(json!({
                "registrations": [{
                    "id": WATCH_REGISTRATION_ID,
                    "method": "workspace/didChangeWatchedFiles",
                    "registerOptions": { "watchers": watchers },
                }],
            })),
        ))
    }

    /// Build (or rebuild) the workspace tier from the client's languages
    /// and each folder's manifest, invalidating the old tier's instances
    /// and the new one's: invalidating instances alone would keep serving
    /// the OLD entries (edited paths and options ignored, added languages
    /// never routed, removed ones never dropped). `rebuildRegistry`.
    fn rebuild_registry(&mut self) {
        for entry in self.router.all().filter(|entry| entry.is_workspace()) {
            self.instances.invalidate(entry);
        }
        let mut workspace = self.config.workspace_entries.clone();
        workspace.extend(self.init_languages.iter().cloned());
        for folder in &self.workspace_folders {
            match read_workspace_manifest(folder) {
                Ok(entries) => workspace.extend(entries),
                Err(error) => log(
                    &self.conn,
                    LOG_WARNING,
                    &format!("tabnas-lsp: workspace manifest ignored: {error}"),
                ),
            }
        }
        self.router.set_workspace(workspace);
        for entry in self.router.all().filter(|entry| entry.is_workspace()) {
            self.instances.invalidate(entry);
        }
    }

    /// `onDidChangeTextDocument`: every change in order against the text
    /// as it stands after the one before (a full replacement followed by
    /// ranged edits included), then the notification's version; stale
    /// structural results are dropped and the document is scheduled.
    fn did_change(&mut self, p: DidChange) {
        let uri = p.text_document.uri;
        let Some(doc) = self.docs.get_mut(&uri) else {
            return;
        };
        doc.apply_changes(
            p.content_changes
                .iter()
                .map(|change| (change.range, change.text.as_str())),
            p.text_document.version,
        );
        self.analyses.remove(&uri);
        self.schedule(&uri);
    }

    /// `onDidChangeWatchedFiles`: a changed manifest rebuilds the tier and
    /// re-analyzes every open document; otherwise a changed grammar file
    /// rebuilds the instances of the workspace entries that load it and
    /// re-analyzes their documents.
    fn did_change_watched_files(&mut self, p: &DidChangeWatchedFiles) {
        let changed: Vec<PathBuf> = p
            .changes
            .iter()
            .filter_map(|change| folder_path_of(&change.uri))
            .map(|path| normalize(&path))
            .collect();
        if changed.is_empty() {
            return;
        }

        let manifest_changed = self
            .workspace_folders
            .iter()
            .any(|folder| changed.contains(&normalize(&folder.join(WORKSPACE_MANIFEST))));
        if manifest_changed {
            self.rebuild_registry();
            let uris: Vec<String> = self.docs.iter().map(|doc| doc.uri.clone()).collect();
            for uri in uris {
                self.analyses.remove(&uri);
                self.schedule(&uri);
            }
            return;
        }

        let mut due = Vec::new();
        for entry in self.router.reloaded_by(&changed) {
            self.instances.invalidate(entry);
            for doc in self.docs.iter() {
                let served = self.router.resolve(&doc.language_id, &doc.uri).entry;
                if served.is_some_and(|served| std::ptr::eq(served, entry)) {
                    due.push(doc.uri.clone());
                }
            }
        }
        for uri in due {
            self.schedule(&uri);
        }
    }

    /// Restart a document's debounce.
    fn schedule(&mut self, uri: &str) {
        self.pending
            .insert(uri.to_string(), Instant::now() + self.config.debounce);
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.pending.values().min().copied()
    }

    /// Analyze every document whose debounce ran out by `now`, earliest
    /// first.
    fn analyze_due(&mut self, now: Instant) -> Result<(), RpcError> {
        let mut due: Vec<(Instant, String)> = self
            .pending
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(uri, at)| (*at, uri.clone()))
            .collect();
        due.sort();
        for (_, uri) in due {
            self.pending.remove(&uri);
            self.run_analysis(&uri)?;
        }
        Ok(())
    }

    /// Analyze every pending document now, without waiting out its
    /// debounce: what the loop does when the deadlines pass, for a host
    /// or a test driving [`Server::handle`] itself.
    pub fn flush(&mut self) -> Result<(), RpcError> {
        let mut due: Vec<(Instant, String)> =
            self.pending.drain().map(|(uri, at)| (at, uri)).collect();
        due.sort();
        for (_, uri) in due {
            self.run_analysis(&uri)?;
        }
        Ok(())
    }

    /// `run(uri)`: one parse of the document as it is now, diagnostics
    /// pushed stamped with the version parsed, and the analysis cached
    /// for that version unless the parse failed.
    fn run_analysis(&mut self, uri: &str) -> Result<(), RpcError> {
        let Some(doc) = self.docs.get(uri) else {
            return Ok(());
        };
        let Some(entry) = self.router.resolve(&doc.language_id, &doc.uri).entry else {
            // No route, so nothing to say about the document: what was
            // said while it had one is withdrawn, since a manifest change
            // can take a language away from an open document.
            if self.published.remove(uri) {
                return self.conn.notify(
                    "textDocument/publishDiagnostics",
                    json!({ "uri": uri, "version": doc.version, "diagnostics": [] }),
                );
            }
            return Ok(());
        };
        if let Some(limit) = self.config.max_document_bytes {
            if doc.text.len() > limit {
                self.analyses.remove(uri);
                self.published.insert(uri.to_string());
                let diagnostic = too_large(entry, doc.text.len(), limit);
                return self.conn.notify(
                    "textDocument/publishDiagnostics",
                    json!({ "uri": uri, "version": doc.version, "diagnostics": [diagnostic] }),
                );
            }
        }
        let inst = match self.instances.get(entry, None) {
            Ok(Some(inst)) => inst,
            Ok(None) => return Ok(()), // quarantined
            Err(error) => {
                log(
                    &self.conn,
                    LOG_ERROR,
                    &format!("tabnas-lsp: grammar load failed: {error}"),
                );
                return Ok(());
            }
        };
        let instances = &self.instances;
        let analysis = with_deadline(&self.deadline, self.config.parse_deadline, || {
            catch_unwind(AssertUnwindSafe(|| analyze(instances, &inst, entry, doc)))
        });
        let failure = match &analysis {
            Err(panic) => Some(panic_message(panic.as_ref()).to_string()),
            Ok(analysis) => internal_failure(analysis),
        };
        if let Some(failure) = failure {
            self.instances.record_failure(entry, None);
            log(
                &self.conn,
                LOG_ERROR,
                &format!(
                    "tabnas-lsp: analysis failed ({}): {failure}",
                    entry.language_id()
                ),
            );
            return Ok(());
        }
        let Ok(analysis) = analysis else {
            return Ok(());
        };
        let version = doc.version;
        // Version-stamped push: stale results never land on newer content.
        if analysis.diagnostics.is_empty() {
            self.published.remove(uri);
        } else {
            self.published.insert(uri.to_string());
        }
        self.conn.notify(
            "textDocument/publishDiagnostics",
            json!({ "uri": uri, "version": version, "diagnostics": analysis.diagnostics }),
        )?;
        if !analysis.failed {
            self.analyses.insert(uri.to_string(), (version, analysis));
        }
        Ok(())
    }

    /// The analysis cached for the document's CURRENT version, if any:
    /// stale structural results are suppressed, never served against
    /// newer content (cached spans are not edit-transformed).
    /// `currentAnalysis`.
    fn current_analysis(&self, uri: &str) -> Option<&Analysis> {
        let doc = self.docs.get(uri)?;
        match self.analyses.get(uri) {
            Some((version, analysis)) if *version == doc.version => Some(analysis),
            _ => None,
        }
    }

    /// `onCompletion`: the continuations at the position, `[]` for an
    /// unknown document, an unrouted one, a quarantined grammar or one
    /// that fails to load (logged, never thrown at the client).
    fn completion(&mut self, p: &PositionParams) -> Value {
        let Some(doc) = self.docs.get(&p.text_document.uri) else {
            return json!([]);
        };
        let Some(entry) = self.router.resolve(&doc.language_id, &doc.uri).entry else {
            return json!([]);
        };
        if self
            .config
            .max_document_bytes
            .is_some_and(|limit| doc.text.len() > limit)
        {
            return json!([]);
        }
        let inst = match self.instances.get(entry, None) {
            Ok(Some(inst)) => inst,
            Ok(None) => return json!([]),
            Err(error) => {
                log(
                    &self.conn,
                    LOG_ERROR,
                    &format!(
                        "tabnas-lsp: grammar load failed ({}): {error}",
                        entry.language_id()
                    ),
                );
                return json!([]);
            }
        };
        let instances = &self.instances;
        let items = with_deadline(&self.deadline, self.config.parse_deadline, || {
            completion(Some(instances), &inst, entry, doc, p.position)
        });
        to_value(&items)
    }

    /// `onHover`: served from the current analysis only; `null` without
    /// one, and `null` wherever the token under the cursor has no
    /// description, which today is everywhere (see [`crate::hover`]).
    fn hover(&self, p: &PositionParams) -> Value {
        let uri = &p.text_document.uri;
        let (Some(doc), Some(analysis)) = (self.docs.get(uri), self.current_analysis(uri)) else {
            return Value::Null;
        };
        let Some(entry) = self.router.resolve(&doc.language_id, &doc.uri).entry else {
            return Value::Null;
        };
        hover(analysis, entry, doc, p.position).map_or(Value::Null, |hover| to_value(&hover))
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
        let languages: Vec<Value> = self
            .router
            .all()
            .map(|entry| {
                json!({
                    "languageId": entry.language_id(),
                    "enabled": entry.is_enabled(),
                    "source": entry.source.as_str(),
                    "quarantined": self.instances.quarantined(entry, None),
                    "lexStream": entry.lex_stream(),
                })
            })
            .collect();
        json!({ "languages": languages })
    }

    /// The negotiated encoding.
    pub fn encoding(&self) -> PositionEncoding {
        self.encoding
    }

    /// Whether `exit` was received.
    pub fn exited(&self) -> bool {
        self.exited
    }

    /// Whether `shutdown` was received: the protocol's exit status is 0
    /// when it came before `exit`, 1 otherwise.
    pub fn shutdown_requested(&self) -> bool {
        self.shutdown
    }

    /// Whether workspace entries may load modules: the configuration's
    /// setting or `initializationOptions.trustWorkspaceModules`.
    pub fn trusts_workspace_modules(&self) -> bool {
        self.trust.load(Ordering::SeqCst)
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn docs(&self) -> &DocumentStore {
        &self.docs
    }

    /// The analysis cached for a document's current version, the one
    /// document symbols, semantic tokens and hover are served from.
    pub fn analysis(&self, uri: &str) -> Option<&Analysis> {
        self.current_analysis(uri)
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
/// done (`Serve` in Go; `startServer` in TypeScript). `true` when the
/// client asked for `shutdown` before the session ended, `false` when
/// it sent `exit` alone or closed the stream: the protocol's exit
/// status is 0 for the first and 1 for the second, which is what the
/// `tabnas-lsp` binary and a generated server report.
pub fn serve(config: Config) -> Result<bool, RpcError> {
    let mut server = Server::new(config, Connection::stdio());
    server.run()?;
    Ok(server.shutdown_requested())
}

/// Read a folder's `.tabnas/lsp.json`: `{"languages": [entry, ...]}`,
/// each entry stamped workspace-sourced with the folder as its sandbox
/// dir. No manifest is the common case and yields no entries (as does
/// one that cannot be read, or whose `languages` is not an array); a
/// malformed one is an error the server reports and never fatal. An
/// entry that is not an object, or whose fields do not have the
/// registry's types, makes the manifest malformed: the TypeScript
/// server would take it and fail later, where this one names it.
pub fn read_workspace_manifest(folder: &Path) -> Result<Vec<Entry>, LoadError> {
    let file = folder.join(WORKSPACE_MANIFEST);
    let Ok(text) = std::fs::read_to_string(&file) else {
        return Ok(Vec::new()); // no manifest: the common case
    };
    let malformed = |message: String| LoadError::new(format!("{}: {message}", file.display()));
    let manifest: Value =
        serde_json::from_str(&text).map_err(|error| malformed(error.to_string()))?;
    let Some(languages) = manifest.get("languages").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let folder_json = Value::from(folder.to_string_lossy().into_owned());
    languages
        .iter()
        .enumerate()
        .map(|(i, language)| {
            let Value::Object(fields) = language else {
                return Err(malformed(format!("languages[{i}] is not an object")));
            };
            let mut fields = fields.clone();
            fields.insert("_source".into(), Value::from("workspace"));
            fields.insert("_dir".into(), folder_json.clone());
            entry_from_fields(fields).map_err(|error| malformed(format!("languages[{i}]: {error}")))
        })
        .collect()
}

/// A workspace folder or watched file as a path: a `file:` URI decoded
/// (`url.fileURLToPath`), anything else as given (`folderPathOf` returns
/// a non-`file:` string unchanged); `None` for the empty string and for
/// a `file:` URI that names no local path (a remote host, an encoded
/// separator, bad percent-encoding).
pub fn folder_path_of(uri_or_path: &str) -> Option<PathBuf> {
    if uri_or_path.is_empty() {
        return None;
    }
    match uri_or_path.strip_prefix("file:") {
        Some(rest) => file_url_to_path(rest),
        None => Some(PathBuf::from(uri_or_path)),
    }
}

/// The path of a `file:` URL (the part after `file:`), as Node's
/// `fileURLToPath` gives it on this platform.
fn file_url_to_path(rest: &str) -> Option<PathBuf> {
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let (host, path) = match rest.strip_prefix("//") {
        Some(after) => match after.find('/') {
            Some(slash) => (&after[..slash], &after[slash..]),
            None => (after, "/"),
        },
        None => ("", rest),
    };
    let host = if host.eq_ignore_ascii_case("localhost") {
        ""
    } else {
        host
    };
    let lower = path.to_ascii_lowercase();
    if lower.contains("%2f") || (cfg!(windows) && lower.contains("%5c")) {
        return None; // an encoded separator names no path
    }
    let decoded = percent_decode(path)?;
    if cfg!(windows) {
        let decoded = decoded.replace('/', "\\");
        if !host.is_empty() {
            return Some(PathBuf::from(format!("\\\\{host}{decoded}")));
        }
        let bytes = decoded.as_bytes();
        // \c:\dir -> c:\dir
        if bytes.len() >= 3
            && bytes[0] == b'\\'
            && bytes[1].is_ascii_alphabetic()
            && bytes[2] == b':'
        {
            return Some(normalize(Path::new(&decoded[1..])));
        }
        return None; // a Windows file URL must name a drive or a host
    }
    if !host.is_empty() {
        return None; // a remote host is not a local path on POSIX
    }
    Some(normalize(Path::new(&decoded)))
}

/// Percent-decode, `None` when a sequence is malformed or the bytes are
/// not UTF-8 (`decodeURIComponent` throws on both).
fn percent_decode(src: &str) -> Option<String> {
    let bytes = src.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = src.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// A path with `.` and `..` resolved lexically (`path.normalize`).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() && !out.has_root() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// One `initializationOptions.languages` item as an entry: stamped
/// workspace-sourced with the default sandbox folder unless it names its
/// own (`Object.assign({_source, _dir}, e, {_scope: null})`: the item's
/// fields win over the stamps), and always session-wide.
fn init_language_entry(language: &Value, default_dir: Option<&Path>) -> Result<Entry, String> {
    let Value::Object(own) = language else {
        return Err("not an object".into());
    };
    let mut fields = Map::new();
    fields.insert("_source".into(), Value::from("workspace"));
    fields.insert(
        "_dir".into(),
        default_dir.map_or(Value::Null, |dir| {
            Value::from(dir.to_string_lossy().into_owned())
        }),
    );
    for (key, value) in own {
        fields.insert(key.clone(), value.clone());
    }
    fields.insert("_scope".into(), Value::Null);
    entry_from_fields(fields).map_err(|error| error.to_string())
}

/// An entry from its JSON fields. A manifest or client entry need not
/// name a package (the TypeScript `normalize` reads a missing `name` as
/// empty), so a missing or null one is the empty name here.
fn entry_from_fields(mut fields: Map<String, Value>) -> Result<Entry, serde_json::Error> {
    if fields.get("name").is_none_or(Value::is_null) {
        fields.insert("name".into(), Value::from(""));
    }
    serde_json::from_value(Value::Object(fields))
}

/// The host's `MakeInstance` behind the server's own gates: a workspace
/// entry's module load is refused unless workspace modules are trusted
/// (read at make time, so `initialize` can grant it), and, with a parse
/// deadline, every instance gets a budget check that stops a parse once
/// the deadline in the shared slot has passed.
fn gated_make_instance(
    inner: MakeInstance,
    trust: Arc<AtomicBool>,
    deadline: Option<Arc<Mutex<Option<Instant>>>>,
) -> MakeInstance {
    Arc::new(move |entry: &Entry| {
        if let Load::Module(name) = entry.load() {
            if entry.is_workspace() && !trust.load(Ordering::SeqCst) {
                return Err(LoadError::new(format!(
                    "workspace entry {} loads module {name}, which runs code from the \
                     workspace. Refused: set trustWorkspaceModules in initializationOptions \
                     to allow it.",
                    entry.language_id()
                )));
            }
        }
        let mut inst = inner(entry)?;
        if let Some(slot) = &deadline {
            let slot = Arc::clone(slot);
            inst.parse_budget(DEADLINE_CHECK_EVERY, move |_| {
                let at = *slot.lock().unwrap_or_else(PoisonError::into_inner);
                at.is_none_or(|at| Instant::now() < at)
            });
        }
        Ok(inst)
    })
}

/// Run `f` with the parse deadline set in the shared slot, and cleared
/// afterwards, when the configuration has one.
fn with_deadline<T>(
    slot: &Mutex<Option<Instant>>,
    limit: Option<Duration>,
    f: impl FnOnce() -> T,
) -> T {
    let Some(limit) = limit else {
        return f();
    };
    *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(Instant::now() + limit);
    let out = f();
    *slot.lock().unwrap_or_else(PoisonError::into_inner) = None;
    out
}

/// An analysis that is the grammar's own code failing: the engine caught
/// a panic (in an action, a matcher, a budget check, a `parser.start`
/// hook) and reported it as an `internal` error, the Rust form of the
/// canonical `analyze` throwing. Where the parse loop catches it, the
/// error is among the recovered ones and the analysis is not `failed`;
/// outside the loop it is the terminal error; either way it is no
/// diagnostic of the document, and the TypeScript server, whose parse
/// rethrows anything that is not a `TabnasError`, publishes nothing for
/// it.
fn internal_failure(analysis: &Analysis) -> Option<String> {
    let internal = analysis
        .errors
        .iter()
        .find(|error| error.code == "internal")?;
    Some(if internal.detail.is_empty() {
        "internal error".to_string()
    } else {
        internal.detail.clone()
    })
}

/// The one diagnostic a document over `Config::max_document_bytes` gets:
/// at its start, naming the sizes, with no engine code.
fn too_large(entry: &Entry, size: usize, limit: usize) -> Diagnostic {
    Diagnostic {
        range: Range::empty(Position::new(0, 0)),
        severity: SEVERITY_ERROR,
        code: None,
        source: format!("tabnas:{}", entry.language_id()),
        message: format!(
            "document is {size} bytes, larger than the {limit}-byte limit: not analyzed"
        ),
        code_description: None,
    }
}

/// The text a panic was raised with, when it was raised with text.
fn panic_message(panic: &(dyn std::any::Any + Send)) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("a panic without a message")
}

/// `window/logMessage`, best effort: a log line that cannot be written
/// is not a reason to stop.
fn log(conn: &Connection, kind: u32, message: &str) {
    let _ = conn.notify(
        "window/logMessage",
        json!({ "type": kind, "message": message }),
    );
}

fn to_value<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn decode<T: serde::de::DeserializeOwned>(params: &Value) -> Result<T, serde_json::Error> {
    T::deserialize(params)
}

// ---------------------------------------------------------------------
// Params, as far as the handlers read them.

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TextDocumentItem {
    uri: String,
    language_id: String,
    version: i64,
    text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DidOpen {
    text_document: TextDocumentItem,
}

#[derive(Deserialize)]
struct TextDocumentIdentifier {
    uri: String,
}

#[derive(Deserialize)]
struct VersionedTextDocumentIdentifier {
    uri: String,
    version: i64,
}

#[derive(Deserialize)]
struct ContentChange {
    #[serde(default)]
    range: Option<Range>,
    text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DidChange {
    text_document: VersionedTextDocumentIdentifier,
    content_changes: Vec<ContentChange>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TextDocumentParams {
    text_document: TextDocumentIdentifier,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PositionParams {
    text_document: TextDocumentIdentifier,
    position: Position,
}

#[derive(Deserialize)]
struct FileEvent {
    uri: String,
}

#[derive(Deserialize)]
struct DidChangeWatchedFiles {
    #[serde(default)]
    changes: Vec<FileEvent>,
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
