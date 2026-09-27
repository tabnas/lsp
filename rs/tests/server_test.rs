// Copyright (c) 2026 Richard Rodger, MIT License

//! The server end to end, in two halves.
//!
//! The built binary, `tabnas-lsp --stdio`, driven by a scripted client
//! over real pipes, as `go/server_test.go` drives `Serve`: initialize,
//! didOpen, didChange (incremental), diagnostics pushed with the right
//! version, semanticTokens/full, documentSymbol, completion, hover,
//! tabnas/status, malformed input, hot reload of a manifest and of a
//! grammar file, shutdown and exit with the protocol's exit status. The
//! client reads the server's frames with its own reader, not the crate's,
//! so a framing defect cannot hide behind a matching one. Where a result
//! is pinned exactly, it is what `ts/bin/tabnas-lsp.js` answers for the
//! same session (captured by running it).
//!
//! The library's [`Server`], driven message by message, for the cases
//! `ts/test/server.test.js` pins at the protocol layer (didChange
//! composition, routing of client and manifest languages, a failing
//! grammar) and for the server's own limits.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tabnas_lsp::jsonrpc::{Connection, Message};
use tabnas_lsp::loaders::Loader;
use tabnas_lsp::{Config, Entry, LoadError, MakeInstance, Server, VERSION};

/// The longest any single wait may take before the test fails: far above
/// the 150 ms debounce, so a slow machine is not a failure.
const WAIT: Duration = Duration::from_secs(20);

/// Longer than the debounce, for the one test that must see nothing
/// happen.
const QUIET: Duration = Duration::from_millis(600);

/// The broken document of `go/server_test.go`.
const BROKEN: &str = "{\"a\":true blah,\"b\":2}";

/// A spec the grammar firewall refuses: `a` names a function that is not
/// an engine builtin (the TypeScript server test's broken grammar).
const REFUSED_SPEC: &str = r##"{"rule":{"top":{"open":[{"s":"#NR","a":"@notabuiltin"}]}}}"##;

fn fixtures() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test/fixtures");
    std::fs::canonicalize(&dir).unwrap_or(dir)
}

fn json_grammar() -> String {
    std::fs::read_to_string(fixtures().join("json-grammar.json")).expect("json-grammar.json")
}

/// A `file:` URI for a local path, percent-encoding what a path may hold
/// that a URI may not.
fn file_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = if text.starts_with('/') {
        text
    } else {
        format!("/{text}")
    };
    let mut uri = String::from("file://");
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' | b':' => {
                uri.push(byte as char)
            }
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri
}

fn range(l0: u32, c0: u32, l1: u32, c1: u32) -> Value {
    json!({"start": {"line": l0, "character": c0}, "end": {"line": l1, "character": c1}})
}

/// A folder under the system temp dir, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "tabnas-lsp-{tag}-{}-{}-{nanos}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        TempDir(std::fs::canonicalize(&dir).expect("canonical temp dir"))
    }

    fn write(&self, relative: &str, text: &str) -> PathBuf {
        let path = self.0.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(&path, text).expect("write");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------
// Framing, independent of the crate's own reader.

fn frame(body: &[u8]) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body);
    out
}

/// One framed message, `None` at the end of the stream.
fn read_frame(reader: &mut impl BufRead) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':')?;
        if name.trim().eq_ignore_ascii_case("content-length") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; length.expect("a frame without Content-Length")];
    reader.read_exact(&mut body).ok()?;
    Some(serde_json::from_slice(&body).expect("a body that is not JSON"))
}

fn is_response(message: &Value, id: &Value) -> bool {
    message.get("method").is_none()
        && message.get("id") == Some(id)
        && (message.get("result").is_some() || message.get("error").is_some())
}

fn is_publish(message: &Value, uri: &str) -> bool {
    message["method"] == "textDocument/publishDiagnostics" && message["params"]["uri"] == uri
}

// ---------------------------------------------------------------------
// The binary, driven over stdio.

/// A scripted client of the built binary.
struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    incoming: Receiver<Value>,
    /// Every message the server has sent, in order.
    seen: Vec<Value>,
    stderr: Arc<Mutex<String>>,
}

impl Client {
    fn spawn(args: &[&str]) -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_tabnas-lsp"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn tabnas-lsp");
        let stdout = child.stdout.take().expect("stdout");
        let (sender, incoming) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Some(message) = read_frame(&mut reader) {
                if sender.send(message).is_err() {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(String::new()));
        let mut child_err = child.stderr.take().expect("stderr");
        let sink = Arc::clone(&stderr);
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = child_err.read_to_string(&mut text);
            sink.lock().unwrap().push_str(&text);
        });
        let stdin = child.stdin.take();
        Client {
            child,
            stdin,
            incoming,
            seen: Vec::new(),
            stderr,
        }
    }

    /// Write raw bytes: frames, or anything else.
    fn write(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        stdin.write_all(bytes).expect("write to the server");
        stdin.flush().expect("flush to the server");
    }

    /// Several messages in ONE write, so the server has them all before
    /// it handles the first: what makes debounce and stale-result checks
    /// deterministic.
    fn send_all(&mut self, messages: &[Value]) {
        let mut bytes = Vec::new();
        for message in messages {
            bytes.extend(frame(&serde_json::to_vec(message).unwrap()));
        }
        self.write(&bytes);
    }

    fn send(&mut self, message: Value) {
        self.send_all(&[message]);
    }

    fn request(&mut self, id: i64, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// The first message the server sent that matches, waiting for more
    /// as needed.
    fn wait_for(&mut self, what: &str, matches: impl Fn(&Value) -> bool) -> Value {
        if let Some(found) = self.seen.iter().find(|message| matches(message)) {
            return found.clone();
        }
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.incoming.recv_timeout(left) {
                Ok(message) => {
                    self.seen.push(message.clone());
                    if matches(&message) {
                        return message;
                    }
                }
                Err(RecvTimeoutError::Timeout) => panic!(
                    "timed out waiting for {what}; the server sent {:#?}\nstderr: {}",
                    self.seen,
                    self.stderr.lock().unwrap()
                ),
                Err(RecvTimeoutError::Disconnected) => panic!(
                    "the server closed its output before {what}; it sent {:#?}\nstderr: {}",
                    self.seen,
                    self.stderr.lock().unwrap()
                ),
            }
        }
    }

    fn response(&mut self, id: i64) -> Value {
        let id = json!(id);
        self.wait_for(&format!("the response to {id}"), |m| is_response(m, &id))
    }

    fn result(&mut self, id: i64) -> Value {
        let response = self.response(id);
        assert!(
            response.get("error").is_none(),
            "request {id} failed: {response}"
        );
        response["result"].clone()
    }

    /// The diagnostics pushed for a document at a version.
    fn diagnostics(&mut self, uri: &str, version: i64) -> Vec<Value> {
        let published = self.wait_for(&format!("diagnostics for {uri} v{version}"), |m| {
            is_publish(m, uri) && m["params"]["version"] == version
        });
        published["params"]["diagnostics"]
            .as_array()
            .cloned()
            .expect("diagnostics array")
    }

    /// A `window/logMessage` whose text contains `needle`.
    fn log_containing(&mut self, needle: &str) -> Value {
        self.wait_for(&format!("a log line with {needle:?}"), |m| {
            m["method"] == "window/logMessage"
                && m["params"]["message"]
                    .as_str()
                    .is_some_and(|text| text.contains(needle))
        })
    }

    /// Everything the server has sent so far, after draining what is
    /// already waiting.
    fn drain(&mut self) -> &[Value] {
        while let Ok(message) = self.incoming.try_recv() {
            self.seen.push(message);
        }
        &self.seen
    }

    /// `initialize`, `initialized`, and the answer to the watcher
    /// registration the server sends when it has a workspace folder.
    fn initialize(&mut self, params: Value) -> Value {
        let has_folder =
            params.get("rootUri").is_some() || params.get("workspaceFolders").is_some();
        self.request(1, "initialize", params);
        let result = self.result(1);
        self.notify("initialized", json!({}));
        if has_folder {
            let register = self.wait_for("the watcher registration", |m| {
                m["method"] == "client/registerCapability"
            });
            self.send(json!({"jsonrpc": "2.0", "id": register["id"], "result": null}));
        }
        result
    }

    fn open(&mut self, uri: &str, language_id: &str, version: i64, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri, "languageId": language_id, "version": version, "text": text}}),
        );
    }

    /// `shutdown` (answered with a null result), then `exit`; the exit
    /// status.
    fn shutdown_and_exit(mut self, id: i64) -> ExitStatus {
        self.request(id, "shutdown", Value::Null);
        let response = self.response(id);
        assert!(
            response.as_object().unwrap().contains_key("result") && response["result"].is_null(),
            "shutdown answers a null result: {response}"
        );
        self.send(json!({"jsonrpc": "2.0", "method": "exit"}));
        self.finish()
    }

    /// Close standard input and wait for the process to end.
    fn finish(mut self) -> ExitStatus {
        drop(self.stdin.take());
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.child.try_wait().expect("wait") {
                return status;
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                panic!(
                    "the server did not exit; stderr: {}",
                    self.stderr.lock().unwrap()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The JSON grammar as a client language served from the fixtures
/// folder: an L2 spec FILE, resolved inside its sandbox folder.
fn jsonf_session() -> Client {
    let mut client = Client::spawn(&["--stdio"]);
    let init = client.initialize(json!({
        "processId": null,
        "rootUri": file_uri(&fixtures()),
        "capabilities": {},
        "initializationOptions": {"languages": [{
            "languageId": "jsonf",
            "extensions": [".jsonf"],
            "load": {"spec": "json-grammar.json"},
        }]},
    }));
    assert_eq!(init["capabilities"]["documentSymbolProvider"], true);
    client
}

#[test]
fn a_scripted_session_over_stdio() {
    let mut client = Client::spawn(&["--stdio"]);
    let root = fixtures();
    client.request(
        1,
        "initialize",
        json!({
            "processId": null,
            "rootUri": file_uri(&root),
            "capabilities": {},
            "initializationOptions": {"languages": [{
                "languageId": "jsonf",
                "extensions": [".jsonf"],
                "load": {"spec": "json-grammar.json"},
            }]},
        }),
    );

    // The initialize response advertises the pipeline's capabilities:
    // those server.js returns, plus the position encoding and server
    // info the Go server adds.
    let init = client.result(1);
    let capabilities = &init["capabilities"];
    assert_eq!(capabilities["positionEncoding"], "utf-16");
    assert_eq!(
        capabilities["textDocumentSync"],
        json!({"openClose": true, "change": 2})
    );
    assert_eq!(
        capabilities["completionProvider"],
        json!({"triggerCharacters": [":", ",", "{", "[", "\""]})
    );
    assert_eq!(capabilities["documentSymbolProvider"], true);
    assert_eq!(capabilities["hoverProvider"], true);
    assert_eq!(
        capabilities["semanticTokensProvider"],
        json!({
            "legend": {
                "tokenTypes": ["string", "number", "comment", "keyword", "operator",
                               "variable", "macro", "type", "property"],
                "tokenModifiers": [],
            },
            "full": true,
        })
    );
    assert_eq!(
        init["serverInfo"],
        json!({"name": "tabnas-lsp", "version": VERSION})
    );

    // With a workspace folder, `initialized` asks the client to watch the
    // grammar sources (hot reload). The client answers; the server
    // ignores the answer.
    client.notify("initialized", json!({}));
    let register = client.wait_for("the watcher registration", |m| {
        m["method"] == "client/registerCapability"
    });
    assert_eq!(
        register["params"]["registrations"][0]["method"],
        "workspace/didChangeWatchedFiles"
    );
    assert_eq!(
        register["params"]["registrations"][0]["registerOptions"]["watchers"],
        json!([
            {"globPattern": "**/.tabnas/lsp.json"},
            {"globPattern": "**/*.{abnf,ebnf,gbnf}"},
            {"globPattern": "**/*.json"},
        ])
    );
    client.send(json!({"jsonrpc": "2.0", "id": register["id"], "result": null}));

    // Version-stamped diagnostics are pushed for the broken document.
    let uri = "file:///t.jsonf";
    client.open(uri, "jsonf", 1, BROKEN);
    let diagnostics = client.diagnostics(uri, 1);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    let d = &diagnostics[0];
    assert_eq!(d["code"], "unexpected");
    assert_eq!(d["range"], range(0, 10, 0, 14));
    assert_eq!(d["severity"], 1);
    assert_eq!(d["source"], "tabnas:jsonf");
    assert_eq!(
        d["codeDescription"],
        json!({"href": "https://tabnas.dev/errors/unexpected"})
    );
    assert!(d["message"]
        .as_str()
        .unwrap()
        .starts_with("unexpected character(s): blah"));

    // The broken document still has structure: an Object symbol.
    client.request(
        2,
        "textDocument/documentSymbol",
        json!({"textDocument": {"uri": uri}}),
    );
    assert_eq!(
        client.result(2),
        json!([{
            "name": "Object",
            "kind": 19,
            "range": range(0, 0, 0, 21),
            "selectionRange": range(0, 0, 0, 0),
            "children": [],
        }])
    );

    // Semantic tokens from the clean lex stream, as TypeScript encodes
    // them.
    client.request(
        3,
        "textDocument/semanticTokens/full",
        json!({"textDocument": {"uri": uri}}),
    );
    assert_eq!(
        client.result(3),
        json!({"data": [
            0, 0, 1, 4, 0, 0, 1, 3, 0, 0, 0, 3, 1, 4, 0, 0, 1, 4, 3, 0, 0, 5, 4, 0, 0,
            0, 4, 1, 4, 0, 0, 1, 3, 0, 0, 0, 3, 1, 4, 0, 0, 1, 1, 1, 0, 0, 1, 1, 4, 0,
        ]})
    );

    // Completion: the engine's continuations at the end of the text.
    client.request(
        4,
        "textDocument/completion",
        json!({"textDocument": {"uri": uri}, "position": {"line": 0, "character": 21}}),
    );
    assert_eq!(
        client.result(4),
        json!([
            {"label": "}", "kind": 24, "detail": "#CB", "insertText": "}"},
            {"label": ",", "kind": 24, "detail": "#CA", "insertText": ","},
        ])
    );

    // Hover has nothing to say yet, in any runtime.
    client.request(
        5,
        "textDocument/hover",
        json!({"textDocument": {"uri": uri}, "position": {"line": 0, "character": 2}}),
    );
    assert_eq!(client.result(5), Value::Null);

    // Status lists the language, with its tier and state.
    client.request(6, "tabnas/status", Value::Null);
    let status = client.result(6);
    let jsonf = status["languages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|language| language["languageId"] == "jsonf")
        .cloned()
        .expect("tabnas/status lists jsonf");
    assert_eq!(
        jsonf,
        json!({"languageId": "jsonf", "enabled": true, "source": "workspace",
               "quarantined": false, "lexStream": "clean"})
    );

    // An incremental change that fixes the document: the new version's
    // diagnostics are pushed, and they are empty.
    client.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri, "version": 2},
               "contentChanges": [{"range": range(0, 9, 0, 14), "text": ""}]}),
    );
    assert_eq!(client.diagnostics(uri, 2), Vec::<Value>::new());

    // Stale structural results are suppressed: a symbol request that
    // arrives after a change and before its analysis gets nothing, never
    // the previous version's spans.
    client.send_all(&[
        json!({"jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
            "textDocument": {"uri": uri, "version": 3},
            "contentChanges": [{"range": range(0, 15, 0, 15), "text": ",\"c\":[1,2]"}]}}),
        json!({"jsonrpc": "2.0", "id": 7, "method": "textDocument/documentSymbol",
               "params": {"textDocument": {"uri": uri}}}),
        json!({"jsonrpc": "2.0", "id": 8, "method": "textDocument/semanticTokens/full",
               "params": {"textDocument": {"uri": uri}}}),
    ]);
    assert_eq!(client.result(7), json!([]));
    assert_eq!(client.result(8), json!({"data": []}));
    assert_eq!(client.diagnostics(uri, 3), Vec::<Value>::new());
    client.request(
        9,
        "textDocument/documentSymbol",
        json!({"textDocument": {"uri": uri}}),
    );
    let symbols = client.result(9);
    assert_eq!(symbols[0]["name"], "Object");
    assert_eq!(symbols[0]["range"], range(0, 0, 0, 26));

    // The debounce: two changes inside it are one analysis, of the later
    // version; the earlier version is never published.
    client.send_all(&[
        json!({"jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
            "textDocument": {"uri": uri, "version": 4},
            "contentChanges": [{"text": "[1,"}]}}),
        json!({"jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
            "textDocument": {"uri": uri, "version": 5},
            "contentChanges": [{"text": "[1,2]"}]}}),
    ]);
    assert_eq!(client.diagnostics(uri, 5), Vec::<Value>::new());
    assert!(
        !client
            .drain()
            .iter()
            .any(|m| is_publish(m, uri) && m["params"]["version"] == 4),
        "a superseded version was analyzed"
    );

    // Malformed input is answered, not fatal: a body that is not JSON
    // gets ParseError with a null id, and the session goes on.
    client.write(&frame(b"{not json"));
    let parse_error = client.wait_for("the ParseError", |m| {
        m.get("id") == Some(&Value::Null) && m["error"]["code"] == -32700
    });
    assert!(parse_error["error"]["message"]
        .as_str()
        .unwrap()
        .starts_with("Parse error"));

    // An unknown request gets MethodNotFound; unknown notifications and
    // cancellation are ignored; params of the wrong shape get
    // InvalidParams.
    client.request(10, "no/such/method", json!({}));
    let unknown = client.response(10);
    assert_eq!(unknown["error"]["code"], -32601);
    assert_eq!(
        unknown["error"]["message"],
        "Unhandled method no/such/method"
    );
    client.notify("$/cancelRequest", json!({"id": 10}));
    client.notify("$/setTrace", json!({"value": "off"}));
    client.notify("no/such/notification", json!({}));
    client.request(11, "textDocument/completion", json!({"textDocument": {}}));
    assert_eq!(client.response(11)["error"]["code"], -32602);

    // Closing clears the document's diagnostics, with no version, and
    // its structural results.
    client.notify(
        "textDocument/didClose",
        json!({"textDocument": {"uri": uri}}),
    );
    let cleared = client.wait_for("the clearing publish", |m| {
        is_publish(m, uri) && m["params"].get("version").is_none()
    });
    assert_eq!(cleared["params"], json!({"uri": uri, "diagnostics": []}));
    client.request(
        12,
        "textDocument/documentSymbol",
        json!({"textDocument": {"uri": uri}}),
    );
    assert_eq!(client.result(12), json!([]));

    // Every request was answered once, and only requests were: the
    // notifications got nothing, the bad body its one error.
    let answered: Vec<Value> = client
        .drain()
        .iter()
        .filter(|m| m.get("method").is_none())
        .map(|m| m["id"].clone())
        .collect();
    let mut expected: Vec<Value> = (1..=12).map(|id| json!(id)).collect();
    expected.insert(9, Value::Null);
    assert_eq!(answered, expected);

    // shutdown answers null; exit after shutdown is status 0.
    assert_eq!(client.shutdown_and_exit(13).code(), Some(0));
}

#[test]
fn exit_without_shutdown_is_status_1() {
    let mut client = Client::spawn(&["--stdio"]);
    client.initialize(json!({"capabilities": {}}));
    client.send(json!({"jsonrpc": "2.0", "method": "exit"}));
    assert_eq!(client.finish().code(), Some(1));
}

#[test]
fn a_closed_stream_ends_the_server_with_the_protocol_status() {
    // After shutdown, a client that just closes the stream gets 0 ...
    let mut client = Client::spawn(&["--stdio"]);
    client.initialize(json!({"capabilities": {}}));
    client.request(2, "shutdown", Value::Null);
    client.response(2);
    assert_eq!(client.finish().code(), Some(0));

    // ... and without it, 1, as vscode-languageserver exits.
    let mut client = Client::spawn(&["--stdio"]);
    client.initialize(json!({"capabilities": {}}));
    assert_eq!(client.finish().code(), Some(1));
}

#[test]
fn the_command_line() {
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tabnas-lsp"))
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("run tabnas-lsp")
    };

    let version = run(&["--version"]);
    assert_eq!(version.status.code(), Some(0));
    let line = String::from_utf8(version.stdout).unwrap();
    assert!(
        line.starts_with(&format!("tabnas-lsp {VERSION} (tabnas engine ")),
        "{line}"
    );

    let help = run(&["--help"]);
    assert_eq!(help.status.code(), Some(0));
    assert!(String::from_utf8(help.stdout)
        .unwrap()
        .contains("usage: tabnas-lsp --stdio"));

    // Anything else is a usage error, on standard error, with nothing on
    // standard output (the protocol's channel).
    for args in [
        &[][..],
        &["--bogus"],
        &["--stdio", "--bogus"],
        &["--stdio", "--stdio"],
        &["--stdio", "--clientProcessId"],
        &["--stdio", "--clientProcessId=me"],
    ] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        assert!(
            String::from_utf8(out.stderr).unwrap().contains("usage:"),
            "{args:?}"
        );
    }

    // A client may add its process id, in either form.
    for args in [
        &["--stdio", "--clientProcessId=4242"][..],
        &["--clientProcessId", "4242", "--stdio"],
    ] {
        let mut client = Client::spawn(args);
        client.initialize(json!({"capabilities": {}}));
        assert_eq!(client.shutdown_and_exit(2).code(), Some(0), "{args:?}");
    }
}

#[test]
fn utf8_is_negotiated_when_offered_and_ranges_count_its_units() {
    // `é` is one UTF-16 unit and two UTF-8 bytes, so the error after it
    // starts one unit later in UTF-8.
    let text = "{\"é\":true blah}";
    for (capabilities, encoding, start) in [
        (json!({}), "utf-16", 10),
        (
            json!({"general": {"positionEncodings": ["utf-16"]}}),
            "utf-16",
            10,
        ),
        (
            json!({"general": {"positionEncodings": ["utf-8", "utf-16"]}}),
            "utf-8",
            11,
        ),
    ] {
        let mut client = Client::spawn(&["--stdio"]);
        let spec: Value = serde_json::from_str(&json_grammar()).unwrap();
        let init = client.initialize(json!({
            "capabilities": capabilities,
            "initializationOptions": {"languages": [{
                "languageId": "jsonf", "extensions": [".jsonf"], "load": {"spec": spec},
            }]},
        }));
        assert_eq!(init["capabilities"]["positionEncoding"], encoding);
        let uri = "untitled:Untitled-1";
        client.open(uri, "jsonf", 1, text);
        let diagnostics = client.diagnostics(uri, 1);
        assert_eq!(
            diagnostics[0]["range"],
            range(0, start, 0, start + 4),
            "{encoding}"
        );
        // Positions from the client are read in the same units: the
        // completion prefix ends at the same place in both.
        client.request(
            2,
            "textDocument/completion",
            json!({"textDocument": {"uri": uri}, "position": {"line": 0, "character": start - 1}}),
        );
        let labels: Vec<Value> = client
            .result(2)
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["label"].clone())
            .collect();
        assert_eq!(labels, vec![json!("}"), json!(",")], "{encoding}");
        assert_eq!(client.shutdown_and_exit(3).code(), Some(0));
    }
}

#[test]
fn a_manifest_added_while_running_routes_its_language() {
    let ws = TempDir::new("manifest");
    ws.write("grammar.json", &json_grammar());
    let mut client = Client::spawn(&["--stdio"]);
    client.initialize(json!({
        "capabilities": {},
        "workspaceFolders": [{"uri": file_uri(&ws.0), "name": "ws"}],
    }));

    // Nothing serves the document yet: nothing is published for it.
    let doc = file_uri(&ws.0.join("x.mydsl"));
    client.open(&doc, "mydsl", 1, BROKEN);
    std::thread::sleep(QUIET);
    client.request(2, "tabnas/status", Value::Null);
    let status = client.result(2);
    assert!(!status["languages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|language| language["languageId"] == "mydsl"));
    assert!(!client.drain().iter().any(|m| is_publish(m, &doc)));

    // A manifest appears and the watcher reports it: the workspace tier
    // is rebuilt and the open document is analyzed.
    let manifest = ws.write(
        ".tabnas/lsp.json",
        r#"{"languages": [{"languageId": "mydsl", "extensions": [".mydsl"],
            "load": {"spec": "grammar.json"}}]}"#,
    );
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({"changes": [{"uri": file_uri(&manifest), "type": 1}]}),
    );
    let diagnostics = client.diagnostics(&doc, 1);
    assert_eq!(diagnostics[0]["code"], "unexpected");
    assert_eq!(diagnostics[0]["source"], "tabnas:mydsl");
    client.request(3, "tabnas/status", Value::Null);
    let status = client.result(3);
    assert!(status["languages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|language| language["languageId"] == "mydsl" && language["source"] == "workspace"));

    // The manifest is removed: the language goes with it.
    std::fs::remove_file(&manifest).unwrap();
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({"changes": [{"uri": file_uri(&manifest), "type": 3}]}),
    );
    client.request(4, "tabnas/status", Value::Null);
    let status = client.result(4);
    assert!(!status["languages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|language| language["languageId"] == "mydsl"));
    assert_eq!(client.shutdown_and_exit(5).code(), Some(0));
}

#[test]
fn an_edited_grammar_file_reloads_its_language() {
    let ws = TempDir::new("reload");
    let grammar = ws.write("grammar.json", REFUSED_SPEC);
    ws.write(
        ".tabnas/lsp.json",
        r#"{"languages": [{"languageId": "mydsl", "extensions": [".mydsl"],
            "load": {"spec": "grammar.json"}}]}"#,
    );
    let mut client = Client::spawn(&["--stdio"]);
    client.initialize(json!({
        "capabilities": {},
        "workspaceFolders": [{"uri": file_uri(&ws.0), "name": "ws"}],
    }));

    // The grammar fails the firewall: logged, nothing published.
    let doc = file_uri(&ws.0.join("x.mydsl"));
    client.open(&doc, "mydsl", 1, BROKEN);
    let failed = client.log_containing("grammar load failed");
    assert_eq!(failed["params"]["type"], 1);
    assert!(failed["params"]["message"]
        .as_str()
        .unwrap()
        .contains("failed the firewall"));

    // The grammar is fixed and the watcher reports it: the instance is
    // rebuilt from the file and the document analyzed.
    std::fs::write(&grammar, json_grammar()).unwrap();
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({"changes": [{"uri": file_uri(&grammar), "type": 2}]}),
    );
    let diagnostics = client.diagnostics(&doc, 1);
    assert_eq!(diagnostics[0]["code"], "unexpected");
    assert_eq!(client.shutdown_and_exit(2).code(), Some(0));
}

#[test]
fn a_workspace_module_needs_trust_and_one_flag_opens_both_gates() {
    for trust in [false, true] {
        let mut client = Client::spawn(&["--stdio"]);
        client.initialize(json!({
            "capabilities": {},
            "initializationOptions": {
                "trustWorkspaceModules": trust,
                "languages": [{"languageId": "modlang", "extensions": [".modlang"],
                               "load": {"module": "@tabnas/no-such-grammar"}}],
            },
        }));
        client.open("file:///x.modlang", "modlang", 1, "1");
        let failed = client.log_containing("grammar load failed");
        let message = failed["params"]["message"].as_str().unwrap();
        // Untrusted, the gate refuses; trusted, both the server's gate and
        // the loader's open, and the load fails only because this binary
        // links no such grammar.
        assert_eq!(
            message.contains("Refused: set trustWorkspaceModules"),
            !trust,
            "{message}"
        );
        assert!(message.contains("@tabnas/no-such-grammar"), "{message}");
        assert_eq!(client.shutdown_and_exit(2).code(), Some(0));
    }
}

#[test]
fn the_json_grammar_session_serves_a_second_document_independently() {
    let mut client = jsonf_session();
    let (a, b) = ("file:///a.jsonf", "file:///b.jsonf");
    client.open(a, "jsonf", 7, "[1,2]");
    client.open(b, "jsonf", 3, BROKEN);
    assert_eq!(client.diagnostics(a, 7), Vec::<Value>::new());
    assert_eq!(client.diagnostics(b, 3).len(), 1);
    // Routing by extension when the client's language id is unknown.
    client.open("file:///c.jsonf", "plaintext", 1, BROKEN);
    assert_eq!(client.diagnostics("file:///c.jsonf", 1).len(), 1);
    assert_eq!(client.shutdown_and_exit(2).code(), Some(0));
}

/// With `fleet`, the bundled registry's linked grammars are served with
/// no configuration at all, routed by language id or by extension; an
/// entry the binary links nothing for is left out of the tier.
#[cfg(feature = "fleet")]
#[test]
fn the_fleet_binary_serves_the_linked_grammars() {
    let mut client = Client::spawn(&["--stdio"]);
    client.initialize(json!({"capabilities": {}}));
    let (good, bad) = ("file:///good.json5", "file:///bad.json5");
    client.open(good, "json5", 1, "{a: 1, b: [2, 'x'], // comment\n}");
    client.open(bad, "plaintext", 1, "{a: 1 b: }");
    assert_eq!(client.diagnostics(good, 1), Vec::<Value>::new());
    let diagnostics = client.diagnostics(bad, 1);
    assert!(!diagnostics.is_empty());
    assert_eq!(diagnostics[0]["source"], "tabnas:json5");

    client.request(2, "tabnas/status", Value::Null);
    let status = client.result(2);
    let languages = status["languages"].as_array().unwrap();
    let json5 = languages
        .iter()
        .find(|language| language["languageId"] == "json5")
        .expect("json5 is bundled");
    assert_eq!(json5["source"], "bundled");
    assert_eq!(json5["enabled"], true);
    // The collision policy keeps an entrenched language off.
    let json = languages
        .iter()
        .find(|language| language["languageId"] == "json")
        .expect("json is bundled");
    assert_eq!(json["enabled"], false);
    // Markdown has no Rust grammar to link.
    assert!(!languages
        .iter()
        .any(|language| language["languageId"] == "markdown"));
    assert_eq!(client.shutdown_and_exit(3).code(), Some(0));
}

// ---------------------------------------------------------------------
// The library's Server, message by message.

/// A writer the test reads back.
#[derive(Clone, Default)]
struct Output(Arc<Mutex<Vec<u8>>>);

impl Write for Output {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Output {
    fn messages(&self) -> Vec<Value> {
        let bytes = self.0.lock().unwrap().clone();
        let mut reader = BufReader::new(&bytes[..]);
        std::iter::from_fn(|| read_frame(&mut reader)).collect()
    }

    fn logs(&self) -> Vec<String> {
        self.messages()
            .iter()
            .filter(|m| m["method"] == "window/logMessage")
            .map(|m| {
                m["params"]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }

    fn publishes(&self, uri: &str) -> Vec<Value> {
        self.messages()
            .into_iter()
            .filter(|m| is_publish(m, uri))
            .map(|m| m["params"].clone())
            .collect()
    }

    fn response(&self, id: i64) -> Value {
        let id = json!(id);
        self.messages()
            .into_iter()
            .find(|m| is_response(m, &id))
            .unwrap_or_else(|| panic!("no response to {id}"))
    }
}

fn library_server(config: Config) -> (Server, Output) {
    let out = Output::default();
    let conn = Connection::new(std::io::empty(), out.clone());
    (Server::new(config, conn), out)
}

fn loader_config() -> Config {
    Config::new(Loader::new().into_make_instance())
}

fn handle(server: &mut Server, message: Message) {
    server.handle(message).expect("write to the client");
}

fn initialize(server: &mut Server, params: Value) {
    handle(
        server,
        Message::request(json!(1), "initialize", Some(params)),
    );
}

fn notify(server: &mut Server, method: &str, params: Value) {
    handle(server, Message::notification(method, Some(params)));
}

fn open(server: &mut Server, uri: &str, language_id: &str, version: i64, text: &str) {
    notify(
        server,
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": language_id, "version": version, "text": text}}),
    );
}

fn language(language_id: &str, extension: &str, load: Value) -> Value {
    json!({"languageId": language_id, "extensions": [extension], "load": load})
}

/// The JSON grammar as a client language, its spec inline: a spec FILE
/// must sit inside its sandbox folder, which for a client language with
/// no workspace folder is the server's working directory.
fn json_language(language_id: &str, extension: &str) -> Value {
    let spec: Value = serde_json::from_str(&json_grammar()).unwrap();
    language(language_id, extension, json!({"spec": spec}))
}

#[test]
fn a_full_replacement_mixed_with_a_ranged_edit_keeps_the_line_index_live() {
    // go TestDidChangeMixedFullAndRanged and its TypeScript twin: the
    // ranged edit is addressed against the replacement's text.
    let (mut server, _) = library_server(loader_config());
    initialize(&mut server, json!({}));
    let uri = "file:///t.jsonf";
    open(&mut server, uri, "jsonf", 1, "aaa\nbbb\nccc");
    notify(
        &mut server,
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri, "version": 2}, "contentChanges": [
            {"text": "x\ny"},
            {"range": range(1, 0, 1, 0), "text": "Z"},
        ]}),
    );
    let doc = server.docs().get(uri).unwrap();
    assert_eq!(doc.text, "x\nZy");
    assert_eq!(doc.version, 2);
}

#[test]
fn consecutive_ranged_edits_still_compose() {
    let (mut server, _) = library_server(loader_config());
    initialize(&mut server, json!({}));
    let uri = "file:///t.jsonf";
    open(&mut server, uri, "jsonf", 1, "[]");
    notify(
        &mut server,
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri, "version": 2}, "contentChanges": [
            {"range": range(0, 1, 0, 1), "text": "1"},
            {"range": range(0, 2, 0, 2), "text": ",2"},
        ]}),
    );
    assert_eq!(server.docs().get(uri).unwrap().text, "[1,2]");
}

#[test]
fn a_client_language_routes_in_every_folder_and_for_non_file_documents() {
    // Client-supplied languages are session-wide: scoping them to the
    // first folder made them dead in every other root, and dead
    // everywhere with no folders at all.
    let lang = json_language("mydsl", ".mydsl");
    let (mut server, _) = library_server(loader_config());
    initialize(
        &mut server,
        json!({
            "workspaceFolders": [{"uri": "file:///ws/alpha"}, {"uri": "file:///ws/beta"}],
            "initializationOptions": {"languages": [lang.clone()]},
        }),
    );
    assert_eq!(
        server.workspace_folders(),
        [PathBuf::from("/ws/alpha"), PathBuf::from("/ws/beta")]
    );
    for uri in ["file:///ws/alpha/x.mydsl", "file:///ws/beta/x.mydsl"] {
        let served = server.router().resolve("mydsl", uri).entry;
        assert_eq!(served.map(Entry::language_id), Some("mydsl"), "{uri}");
    }

    let (mut server, _) = library_server(loader_config());
    initialize(
        &mut server,
        json!({"initializationOptions": {"languages": [lang]}}),
    );
    for uri in ["file:///anywhere/x.mydsl", "untitled:Untitled-1"] {
        let served = server.router().resolve("mydsl", uri).entry;
        assert_eq!(served.map(Entry::language_id), Some("mydsl"), "{uri}");
    }
}

#[test]
fn a_folder_manifest_stays_scoped_to_its_folder() {
    let alpha = TempDir::new("scoped");
    alpha.write(
        ".tabnas/lsp.json",
        &json!({"languages": [json_language("scoped", ".scoped")]}).to_string(),
    );
    let alpha_uri = file_uri(&alpha.0);
    let (mut server, _) = library_server(loader_config());
    initialize(
        &mut server,
        json!({"workspaceFolders": [{"uri": alpha_uri}, {"uri": "file:///ws/beta"}]}),
    );
    let inside = server
        .router()
        .resolve("scoped", &format!("{alpha_uri}/x.scoped"))
        .entry;
    assert_eq!(inside.map(Entry::language_id), Some("scoped"));
    let outside = server
        .router()
        .resolve("scoped", "file:///ws/beta/x.scoped")
        .entry;
    assert!(
        outside.is_none(),
        "a folder manifest must not capture another folder"
    );
}

#[test]
fn the_root_uri_is_the_folder_when_no_workspace_folders_are_sent() {
    let (mut server, _) = library_server(loader_config());
    initialize(&mut server, json!({"rootUri": "file:///ws/root%20dir"}));
    assert_eq!(server.workspace_folders(), [PathBuf::from("/ws/root dir")]);
}

#[test]
fn a_malformed_manifest_is_reported_and_never_fatal() {
    let ws = TempDir::new("malformed");
    ws.write(".tabnas/lsp.json", "{ not json");
    let (mut server, out) = library_server(loader_config());
    initialize(
        &mut server,
        json!({"workspaceFolders": [{"uri": file_uri(&ws.0)}]}),
    );
    assert!(out.response(1)["result"]["capabilities"].is_object());
    assert!(
        out.logs()
            .iter()
            .any(|line| line.contains("workspace manifest ignored")),
        "{:?}",
        out.logs()
    );
}

#[test]
fn completion_survives_a_grammar_that_fails_to_load() {
    // instances.get passes a load failure on (after counting it); the
    // completion handler logs it and answers [], never an error on every
    // keystroke of a broken grammar.
    let (mut server, out) = library_server(loader_config());
    let refused: Value = serde_json::from_str(REFUSED_SPEC).unwrap();
    initialize(
        &mut server,
        json!({"initializationOptions": {"languages": [
            language("broken", ".broken", json!({"spec": refused})),
        ]}}),
    );
    let uri = "file:///t.broken";
    open(&mut server, uri, "broken", 1, "1");
    handle(
        &mut server,
        Message::request(
            json!(2),
            "textDocument/completion",
            Some(json!({"textDocument": {"uri": uri}, "position": {"line": 0, "character": 1}})),
        ),
    );
    assert_eq!(out.response(2)["result"], json!([]));
    assert!(
        out.logs()
            .iter()
            .any(|line| line.contains("grammar load failed")),
        "{:?}",
        out.logs()
    );
}

#[test]
fn a_grammar_that_keeps_failing_is_quarantined_and_the_rest_are_served() {
    let (mut server, out) = library_server(loader_config());
    let refused: Value = serde_json::from_str(REFUSED_SPEC).unwrap();
    initialize(
        &mut server,
        json!({"initializationOptions": {"languages": [
            language("broken", ".broken", json!({"spec": refused})),
            json_language("jsonf", ".jsonf"),
        ]}}),
    );
    let uri = "file:///t.broken";
    open(&mut server, uri, "broken", 1, "1");
    server.flush().unwrap();
    for version in 2..=4 {
        notify(
            &mut server,
            "textDocument/didChange",
            json!({"textDocument": {"uri": uri, "version": version},
                   "contentChanges": [{"text": "2"}]}),
        );
        server.flush().unwrap();
    }
    // Three failed loads, then quarantine: the fourth analysis does not
    // try again.
    let failures = out
        .logs()
        .iter()
        .filter(|line| line.contains("grammar load failed"))
        .count();
    assert_eq!(failures, 3);
    let status = server.status();
    let broken = status["languages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|language| language["languageId"] == "broken")
        .cloned()
        .unwrap();
    assert_eq!(broken["quarantined"], true);
    assert!(out.publishes(uri).is_empty());

    // Another grammar is served as if nothing happened.
    open(&mut server, "file:///ok.jsonf", "jsonf", 1, BROKEN);
    server.flush().unwrap();
    let published = out.publishes("file:///ok.jsonf");
    assert_eq!(published.len(), 1);
    assert_eq!(published[0]["diagnostics"][0]["code"], "unexpected");
}

#[test]
fn a_grammar_whose_code_panics_is_counted_and_publishes_nothing() {
    // A host grammar whose own code panics mid-parse: the engine reports
    // it as `internal`, which is the canonical analyze THROWING, so the
    // server logs it, counts it toward quarantine and publishes nothing.
    let spec = json_grammar();
    let (_, make_good) = tabnas_lsp::loaders::entry_from_spec_json("jsonf", &[".jsonf"], &spec);
    let make: MakeInstance = Arc::new(move |entry: &Entry| {
        let mut inst = make_good(entry)?;
        inst.parse_budget(1, |_| panic!("the grammar's own code failed"));
        Ok(inst)
    });
    // Client languages are workspace-sourced, so the host's module needs
    // trust (the next test covers the gate).
    let mut config = Config::new(make);
    config.trust_workspace_modules = true;
    let (mut server, out) = library_server(config);
    initialize(
        &mut server,
        json!({"initializationOptions": {"languages": [
            language("jsonf", ".jsonf", json!({"module": "host"})),
        ]}}),
    );
    let uri = "file:///t.jsonf";
    open(&mut server, uri, "jsonf", 1, "[1,2,3]");
    server.flush().unwrap();
    assert!(out.publishes(uri).is_empty(), "{:?}", out.publishes(uri));
    let logs = out.logs();
    assert!(
        logs.iter()
            .any(|line| line.starts_with("tabnas-lsp: analysis failed (jsonf)")),
        "{logs:?}"
    );
    let entry = server.router().resolve("jsonf", uri).entry.unwrap().clone();
    assert_eq!(server.instances().failures(&entry, None), 1);
}

#[test]
fn the_trust_gate_wraps_the_hosts_make_instance() {
    let calls = Arc::new(AtomicUsize::new(0));
    let config = |trust: bool| {
        let calls = Arc::clone(&calls);
        let spec = json_grammar();
        let (_, make_good) = tabnas_lsp::loaders::entry_from_spec_json("jsonf", &[".jsonf"], &spec);
        let make: MakeInstance = Arc::new(move |entry: &Entry| {
            calls.fetch_add(1, Ordering::SeqCst);
            make_good(entry)
        });
        let mut config = Config::new(make);
        config.trust_workspace_modules = trust;
        config
    };
    let init = |trust_option: bool| {
        json!({"initializationOptions": {
            "trustWorkspaceModules": trust_option,
            "languages": [language("jsonf", ".jsonf", json!({"module": "host"}))],
        }})
    };
    let uri = "file:///t.jsonf";

    // Untrusted: refused before the host's callback runs.
    let (mut server, out) = library_server(config(false));
    initialize(&mut server, init(false));
    open(&mut server, uri, "jsonf", 1, BROKEN);
    server.flush().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!server.trusts_workspace_modules());
    assert!(
        out.logs().iter().any(|line| line.contains(
            "workspace entry jsonf loads module host, which runs code from the workspace. \
             Refused: set trustWorkspaceModules in initializationOptions to allow it."
        )),
        "{:?}",
        out.logs()
    );

    // Trusted by the initialization option, or by the host's
    // configuration: served.
    for (configured, option) in [(false, true), (true, false)] {
        let before = calls.load(Ordering::SeqCst);
        let (mut server, out) = library_server(config(configured));
        initialize(&mut server, init(option));
        assert!(server.trusts_workspace_modules());
        open(&mut server, uri, "jsonf", 1, BROKEN);
        server.flush().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), before + 1);
        assert_eq!(
            out.publishes(uri)[0]["diagnostics"][0]["code"],
            "unexpected"
        );
    }

    // A shared flag opens the host's own gate with the server's.
    let shared = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let conn = Connection::new(std::io::empty(), Output::default());
    let mut server = Server::with_trust(config(false), conn, Arc::clone(&shared));
    initialize(&mut server, init(true));
    assert!(shared.load(Ordering::SeqCst));
}

#[test]
fn a_document_over_the_size_limit_gets_one_diagnostic_and_no_parse() {
    let mut config = loader_config();
    config.max_document_bytes = Some(8);
    let (mut server, out) = library_server(config);
    initialize(
        &mut server,
        json!({"initializationOptions": {"languages": [json_language("jsonf", ".jsonf")]}}),
    );
    let (small, large) = ("file:///small.jsonf", "file:///large.jsonf");
    open(&mut server, small, "jsonf", 1, "[1,2]");
    open(&mut server, large, "jsonf", 1, "[1,2,3,4,5]");
    server.flush().unwrap();
    assert_eq!(out.publishes(small)[0]["diagnostics"], json!([]));
    let published = out.publishes(large);
    assert_eq!(
        published[0]["diagnostics"],
        json!([{
            "range": range(0, 0, 0, 0),
            "severity": 1,
            "source": "tabnas:jsonf",
            "message": "document is 11 bytes, larger than the 8-byte limit: not analyzed",
        }])
    );
    assert_eq!(published[0]["version"], 1);
    // No analysis to serve structure from, and no completion either.
    assert!(server.analysis(large).is_none());
    handle(
        &mut server,
        Message::request(
            json!(2),
            "textDocument/completion",
            Some(json!({"textDocument": {"uri": large}, "position": {"line": 0, "character": 3}})),
        ),
    );
    assert_eq!(out.response(2)["result"], json!([]));
}

#[test]
fn a_parse_deadline_stops_a_long_parse() {
    let mut config = loader_config();
    config.parse_deadline = Some(Duration::ZERO);
    let (mut server, out) = library_server(config);
    initialize(
        &mut server,
        json!({"initializationOptions": {"languages": [json_language("jsonf", ".jsonf")]}}),
    );
    let uri = "file:///long.jsonf";
    let text = format!("[{}0]", "0,".repeat(2000));
    open(&mut server, uri, "jsonf", 1, &text);
    server.flush().unwrap();
    let published = out.publishes(uri);
    let codes: Vec<&Value> = published[0]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| &d["code"])
        .collect();
    assert_eq!(codes, [&json!("cancel")], "{published:?}");
}

#[test]
fn the_debounce_is_the_configured_one_and_flush_runs_what_is_due() {
    let mut config = loader_config();
    config.debounce = Duration::from_secs(3600);
    let (mut server, out) = library_server(config);
    initialize(
        &mut server,
        json!({"initializationOptions": {"languages": [json_language("jsonf", ".jsonf")]}}),
    );
    let uri = "file:///t.jsonf";
    open(&mut server, uri, "jsonf", 1, BROKEN);
    assert_eq!(server.pending(), 1);
    assert!(out.publishes(uri).is_empty());
    server.flush().unwrap();
    assert_eq!(server.pending(), 0);
    assert_eq!(out.publishes(uri)[0]["version"], 1);
    assert!(server.analysis(uri).is_some());
    // Closing drops a pending analysis with the document.
    notify(
        &mut server,
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri, "version": 2}, "contentChanges": [{"text": "[]"}]}),
    );
    assert!(server.analysis(uri).is_none(), "stale after a change");
    notify(
        &mut server,
        "textDocument/didClose",
        json!({"textDocument": {"uri": uri}}),
    );
    assert_eq!(server.pending(), 0);
    server.flush().unwrap();
    assert!(!out
        .publishes(uri)
        .iter()
        .any(|params| params["version"] == 2));
}

#[test]
fn a_host_make_instance_error_is_a_load_failure() {
    // The bundled tier comes from the host's configuration: a host (aless)
    // serving its own parsers, one of which fails to build.
    let make: MakeInstance = Arc::new(|entry: &Entry| {
        Err(LoadError::new(format!(
            "no grammar for {}",
            entry.language_id()
        )))
    });
    let mut entry = Entry::new("@host/x");
    entry.language_id = Some("x".into());
    entry.extensions = vec![".x".into()];
    let mut config = Config::new(make);
    config.entries.push(entry);
    let (mut server, out) = library_server(config);
    initialize(&mut server, json!({}));
    open(&mut server, "file:///a.x", "x", 1, "text");
    server.flush().unwrap();
    assert!(
        out.logs()
            .iter()
            .any(|line| line == "tabnas-lsp: grammar load failed: no grammar for x"),
        "{:?}",
        out.logs()
    );
    let status = server.status();
    assert_eq!(
        status["languages"][0],
        json!({"languageId": "x", "enabled": true, "source": "bundled",
               "quarantined": false, "lexStream": "clean"})
    );
}
