// Copyright (c) 2026 Richard Rodger, MIT License

//! JSON-RPC 2.0 framing over stdio: `Content-Length` headers, one JSON
//! body per message. Mirrors `go/jsonrpc.go`, deliberately
//! dependency-free beyond serde: the whole of what an LSP transport
//! needs is here, so a generated Rust server builds with nothing beyond
//! the engine and this crate. No async runtime: a [`Connection`] owns a
//! reader thread that frames messages into a channel and a writer
//! guarded by a lock, and the server's single loop receives from the
//! channel with a timeout, which is what lets it debounce without a
//! second thread of its own.
//!
//! Malformed input never panics and never ends the session on its own:
//! a body that is not JSON is an [`RpcError::Json`] (the server answers
//! `ParseError` with a null id, as JSON-RPC 2.0 specifies), JSON that is
//! not a message object is an [`RpcError::Invalid`] (`InvalidRequest`),
//! and a header block without a usable `Content-Length` is an
//! [`RpcError::Frame`]; the reader reports each and reads on from the
//! next header block, which is what the canonical transport
//! (`vscode-jsonrpc`) does. Only the stream itself failing or ending
//! stops the reader. A body is read as it arrives rather than allocated
//! from its declared length, so a header claiming a huge body costs
//! nothing until the bytes exist.

use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::mpsc::{self, Receiver};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `ParseError`: the body was not JSON.
pub const PARSE_ERROR: i64 = -32700;
/// `InvalidRequest`.
pub const INVALID_REQUEST: i64 = -32600;
/// `MethodNotFound`: an unknown request (unknown notifications are
/// ignored, per the protocol).
pub const METHOD_NOT_FOUND: i64 = -32601;
/// `InvalidParams`.
pub const INVALID_PARAMS: i64 = -32602;
/// `InternalError`.
pub const INTERNAL_ERROR: i64 = -32603;

fn version() -> String {
    "2.0".to_string()
}

/// A JSON-RPC message: a request (`id` and `method`), a notification
/// (`method` alone), or a response (`id` with `result` or `error`).
/// `result` is emitted whenever it is set, `null` included: a null
/// result is a valid response and an absent one is not.
///
/// Reading keeps a member that is present with a `null` value apart from
/// an absent one: `"result": null` is `Some(Value::Null)` (a response),
/// and `"id": null` is `Some(Value::Null)` (the id a `ParseError` carries).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    #[serde(default = "version")]
    pub jsonrpc: String,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub id: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub params: Option<Value>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
}

/// A member that is present, `null` included. Absent members take the
/// field's `default`, `None`.
fn present<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

/// The `error` member of a response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl Message {
    fn blank() -> Message {
        Message {
            jsonrpc: version(),
            id: None,
            method: None,
            params: None,
            result: None,
            error: None,
        }
    }

    pub fn request(id: Value, method: impl Into<String>, params: Option<Value>) -> Message {
        Message {
            id: Some(id),
            method: Some(method.into()),
            params,
            ..Message::blank()
        }
    }

    pub fn notification(method: impl Into<String>, params: Option<Value>) -> Message {
        Message {
            method: Some(method.into()),
            params,
            ..Message::blank()
        }
    }

    /// A successful response; pass `Value::Null` for a null result.
    pub fn response(id: Value, result: Value) -> Message {
        Message {
            id: Some(id),
            result: Some(result),
            ..Message::blank()
        }
    }

    pub fn error_response(id: Value, code: i64, message: impl Into<String>) -> Message {
        Message {
            id: Some(id),
            error: Some(ResponseError {
                code,
                message: message.into(),
                data: None,
            }),
            ..Message::blank()
        }
    }

    pub fn is_request(&self) -> bool {
        self.id.is_some() && self.method.is_some()
    }

    pub fn is_notification(&self) -> bool {
        self.id.is_none() && self.method.is_some()
    }

    pub fn is_response(&self) -> bool {
        self.method.is_none() && (self.result.is_some() || self.error.is_some())
    }
}

/// A transport failure: the stream, a malformed frame, a body that is
/// not JSON, or JSON that is not a message.
#[derive(Debug)]
pub enum RpcError {
    /// The stream failed or ended mid-message. Fatal to the connection.
    Io(std::io::Error),
    /// A header block without a usable `Content-Length`. The reader
    /// reads on from the next header block.
    Frame(String),
    /// A body that is not JSON: answered with [`PARSE_ERROR`].
    Json(serde_json::Error),
    /// JSON that is not a message object (a batch, a scalar, a field of
    /// the wrong type): answered with [`INVALID_REQUEST`], with the id
    /// when one could be read.
    Invalid { id: Option<Value>, message: String },
}

impl RpcError {
    /// Whether the connection can go on after this error: every framing
    /// or content error can, a stream error cannot.
    pub fn is_recoverable(&self) -> bool {
        !matches!(self, RpcError::Io(_))
    }

    /// The error response this failure warrants, if any: `ParseError`
    /// with a null id for a body that is not JSON, `InvalidRequest` for
    /// JSON that is not a message. A frame or stream error has no
    /// request to answer.
    pub fn response(&self) -> Option<Message> {
        match self {
            RpcError::Json(error) => Some(Message::error_response(
                Value::Null,
                PARSE_ERROR,
                format!("Parse error: {error}"),
            )),
            RpcError::Invalid { id, message } => Some(Message::error_response(
                id.clone().unwrap_or(Value::Null),
                INVALID_REQUEST,
                format!("Invalid request: {message}"),
            )),
            RpcError::Io(_) | RpcError::Frame(_) => None,
        }
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RpcError::Io(error) => write!(f, "jsonrpc: {error}"),
            RpcError::Frame(message) => write!(f, "jsonrpc: {message}"),
            RpcError::Json(error) => write!(f, "jsonrpc: bad JSON-RPC body: {error}"),
            RpcError::Invalid { message, .. } => write!(f, "jsonrpc: invalid message: {message}"),
        }
    }
}

impl std::error::Error for RpcError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RpcError::Io(error) => Some(error),
            RpcError::Frame(_) | RpcError::Invalid { .. } => None,
            RpcError::Json(error) => Some(error),
        }
    }
}

impl From<std::io::Error> for RpcError {
    fn from(error: std::io::Error) -> Self {
        RpcError::Io(error)
    }
}

impl From<serde_json::Error> for RpcError {
    fn from(error: serde_json::Error) -> Self {
        RpcError::Json(error)
    }
}

/// Read one framed message: headers up to a blank line (`Content-Length`
/// is required, case-insensitive; other headers are skipped), then
/// exactly that many bytes of body. `Ok(None)` at a clean end of stream,
/// which is an end before any byte of a header block; an end anywhere
/// later is an [`RpcError::Io`].
///
/// A missing or unparsable `Content-Length` is an [`RpcError::Frame`]
/// after the whole header block is consumed, so the next call starts at
/// the next header block. A body that is not JSON is an
/// [`RpcError::Json`]; JSON that is not a message object is an
/// [`RpcError::Invalid`]. Either way the body was consumed in full and
/// the stream stays framed.
pub fn read_message(reader: &mut dyn BufRead) -> Result<Option<Message>, RpcError> {
    let mut length: Option<Result<u64, String>> = None;
    let mut line = Vec::new();
    let mut started = false;
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            if started {
                return Err(RpcError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "stream ended inside a header block",
                )));
            }
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches(['\r', '\n']);
        if text.is_empty() {
            if started {
                break; // end of headers
            }
            // Blank lines between messages are tolerated, as Go's
            // reader tolerates them after a message's body.
            continue;
        }
        started = true;
        let Some((name, value)) = text.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            let value = value.trim();
            length = Some(
                value
                    .parse::<u64>()
                    .map_err(|_| format!("bad Content-Length: {value:?}")),
            );
        }
    }
    let length = match length {
        None => return Err(RpcError::Frame("missing Content-Length header".into())),
        Some(Err(message)) => return Err(RpcError::Frame(message)),
        Some(Ok(length)) => length,
    };
    // Read as the bytes arrive: no allocation sized by the header.
    let mut body = Vec::new();
    reader.take(length).read_to_end(&mut body)?;
    if (body.len() as u64) < length {
        return Err(RpcError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            format!(
                "stream ended {} bytes into a {length}-byte body",
                body.len()
            ),
        )));
    }
    let value: Value = serde_json::from_slice(&body)?;
    message_of(value).map(Some)
}

/// A parsed body as a message, or why it is not one.
fn message_of(value: Value) -> Result<Message, RpcError> {
    if !value.is_object() {
        let kind = match value {
            Value::Array(_) => "a batch (an array), which this server does not accept",
            _ => "not an object",
        };
        return Err(RpcError::Invalid {
            id: None,
            message: kind.into(),
        });
    }
    let id = value
        .get("id")
        .filter(|id| id.is_string() || id.is_number())
        .cloned();
    serde_json::from_value(value).map_err(|error| RpcError::Invalid {
        id,
        message: error.to_string(),
    })
}

/// Write one framed message: `Content-Length: <n>\r\n\r\n` and the body,
/// then flush.
pub fn write_message(writer: &mut dyn Write, message: &Message) -> Result<(), RpcError> {
    let body = serde_json::to_vec(message)?;
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(&body);
    writer.write_all(&frame)?;
    writer.flush()?;
    Ok(())
}

/// What a receive yielded. A `Received` lives for one turn of the
/// server loop, so the message is carried inline rather than boxed per
/// message for the sake of the two small variants.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Received {
    Message(Message),
    /// The timeout elapsed with nothing to read.
    Timeout,
    /// The client closed the stream.
    Closed,
}

/// A framed connection: a reader thread feeding a channel, and a
/// lock-guarded writer.
pub struct Connection {
    incoming: Receiver<Result<Message, RpcError>>,
    writer: Mutex<Box<dyn Write + Send>>,
}

impl Connection {
    /// Standard input and output.
    pub fn stdio() -> Connection {
        Connection::new(std::io::stdin(), std::io::stdout())
    }

    /// A connection over any pair of streams; spawns the reader thread.
    ///
    /// The thread frames messages into the channel until the stream ends
    /// or fails (the channel then closes, which a receive reports as
    /// [`Received::Closed`], after the stream error itself when there was
    /// one) or the connection is dropped. Recoverable errors
    /// ([`RpcError::is_recoverable`]) are passed on and reading goes on.
    pub fn new(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
    ) -> Connection {
        let (sender, incoming) = mpsc::channel();
        let spawned = thread::Builder::new()
            .name("tabnas-lsp-reader".into())
            .spawn(move || {
                let mut reader = BufReader::new(reader);
                loop {
                    let next = match read_message(&mut reader) {
                        Ok(Some(message)) => Ok(message),
                        Ok(None) => break,
                        Err(error) => Err(error),
                    };
                    let fatal = matches!(next, Err(ref error) if !error.is_recoverable());
                    if sender.send(next).is_err() || fatal {
                        break;
                    }
                }
            });
        if let Err(error) = spawned {
            // No reader: the connection reports the failure once and is
            // closed thereafter.
            let (sender, closed) = mpsc::channel();
            let _ = sender.send(Err(RpcError::Io(error)));
            return Connection {
                incoming: closed,
                writer: Mutex::new(Box::new(writer)),
            };
        }
        Connection {
            incoming,
            writer: Mutex::new(Box::new(writer)),
        }
    }

    /// The next message, blocking; `Closed` at end of stream.
    pub fn recv(&self) -> Result<Received, RpcError> {
        match self.incoming.recv() {
            Ok(Ok(message)) => Ok(Received::Message(message)),
            Ok(Err(error)) => Err(error),
            Err(_) => Ok(Received::Closed),
        }
    }

    /// The next message, or `Timeout` after `timeout` with none: the
    /// server's debounce clock.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Received, RpcError> {
        match self.incoming.recv_timeout(timeout) {
            Ok(Ok(message)) => Ok(Received::Message(message)),
            Ok(Err(error)) => Err(error),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ok(Received::Timeout),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Ok(Received::Closed),
        }
    }

    /// Write one message under the writer lock.
    pub fn send(&self, message: &Message) -> Result<(), RpcError> {
        let mut writer = self
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        write_message(&mut *writer, message)
    }

    /// Answer a request. A null result is emitted as `null`.
    pub fn respond(&self, id: Value, result: Value) -> Result<(), RpcError> {
        self.send(&Message::response(id, result))
    }

    /// Answer a request with an error.
    pub fn respond_error(
        &self,
        id: Value,
        code: i64,
        message: impl Into<String>,
    ) -> Result<(), RpcError> {
        self.send(&Message::error_response(id, code, message))
    }

    /// Send a server-initiated notification.
    pub fn notify(&self, method: impl Into<String>, params: Value) -> Result<(), RpcError> {
        self.send(&Message::notification(method, Some(params)))
    }
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connection").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn messages_classify_and_serialize_in_the_wire_shape() {
        let request = Message::request(json!(1), "initialize", Some(json!({})));
        assert!(request.is_request());
        assert!(!request.is_notification());
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["jsonrpc"], "2.0");
        assert_eq!(json["method"], "initialize");
        assert!(json.get("result").is_none());

        let notification = Message::notification("exit", None);
        assert!(notification.is_notification());
        let json = serde_json::to_value(&notification).unwrap();
        assert!(json.get("id").is_none());
        assert!(json.get("params").is_none());

        // A null result is emitted; an absent one is not a response.
        let response = Message::response(json!(2), Value::Null);
        assert!(response.is_response());
        let text = serde_json::to_string(&response).unwrap();
        assert!(text.contains("\"result\":null"), "{text}");

        let error = Message::error_response(json!(3), METHOD_NOT_FOUND, "method not found: x");
        assert!(error.is_response());
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["error"]["code"], -32601);
        assert!(json["error"].get("data").is_none());

        // A null result reads back as present: a response.
        let back: Message = serde_json::from_str(&text).unwrap();
        assert_eq!(back, response);
        assert!(back.is_response());
        let absent: Message = serde_json::from_str(r#"{"id": 2}"#).unwrap();
        assert!(!absent.is_response());
        assert!(!absent.is_request());

        // An incoming body without `jsonrpc` still reads.
        let incoming: Message = serde_json::from_str(r#"{"id": 9, "method": "shutdown"}"#).unwrap();
        assert_eq!(incoming.jsonrpc, "2.0");
        assert!(incoming.is_request());
    }

    #[test]
    fn errors_display_their_source() {
        let error = RpcError::Frame("missing Content-Length header".into());
        assert_eq!(error.to_string(), "jsonrpc: missing Content-Length header");
        let json: RpcError = serde_json::from_str::<Value>("nope").unwrap_err().into();
        assert!(json.to_string().starts_with("jsonrpc: bad JSON-RPC body"));
        let io: RpcError = std::io::Error::other("closed").into();
        assert!(io.to_string().contains("closed"));
        assert!(std::error::Error::source(&io).is_some());
    }

    fn frame(body: &str) -> String {
        format!("Content-Length: {}\r\n\r\n{body}", body.len())
    }

    fn reader(src: &str) -> std::io::Cursor<Vec<u8>> {
        std::io::Cursor::new(src.as_bytes().to_vec())
    }

    #[test]
    fn a_written_message_reads_back() {
        let mut out = Vec::new();
        let sent = Message::request(json!(7), "textDocument/hover", Some(json!({"a": "é𝄞"})));
        write_message(&mut out, &sent).unwrap();
        let text = String::from_utf8(out.clone()).unwrap();
        let body = serde_json::to_string(&sent).unwrap();
        // The length counts bytes, not characters.
        assert!(text.starts_with(&format!("Content-Length: {}\r\n\r\n", body.len())));
        let mut input = std::io::Cursor::new(out);
        assert_eq!(read_message(&mut input).unwrap(), Some(sent));
        assert!(read_message(&mut input).unwrap().is_none(), "clean end");
    }

    #[test]
    fn headers_are_case_insensitive_and_others_are_skipped() {
        let body = r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#;
        let src = format!(
            "content-type: application/vscode-jsonrpc; charset=utf-8\r\nCONTENT-LENGTH:  {}  \r\nnot a header\r\n\r\n{body}",
            body.len()
        );
        let message = read_message(&mut reader(&src)).unwrap().unwrap();
        assert_eq!(message.method.as_deref(), Some("initialized"));
        assert!(message.is_notification());
        // Bare \n line ends are read too.
        let src = format!("Content-Length: {}\n\n{body}", body.len());
        assert!(read_message(&mut reader(&src)).unwrap().is_some());
    }

    #[test]
    fn a_frame_without_a_length_is_reported_and_reading_goes_on() {
        let good = r#"{"jsonrpc":"2.0","id":1,"method":"shutdown"}"#;
        let src = format!("X-Other: 1\r\n\r\n{}", frame(good));
        let mut input = reader(&src);
        let error = read_message(&mut input).unwrap_err();
        assert!(matches!(error, RpcError::Frame(_)), "{error}");
        assert!(error.is_recoverable());
        assert!(error.response().is_none());
        assert_eq!(
            read_message(&mut input).unwrap().unwrap().method.as_deref(),
            Some("shutdown")
        );

        let src = format!("Content-Length: -3\r\n\r\n{}", frame(good));
        let mut input = reader(&src);
        let error = read_message(&mut input).unwrap_err();
        assert!(error.to_string().contains("bad Content-Length"), "{error}");
        assert!(read_message(&mut input).unwrap().is_some());
    }

    #[test]
    fn a_body_that_is_not_json_is_a_parse_error_and_the_stream_stays_framed() {
        let src = format!(
            "{}{}",
            frame("{not json"),
            frame(r#"{"jsonrpc":"2.0","method":"exit"}"#)
        );
        let mut input = reader(&src);
        let error = read_message(&mut input).unwrap_err();
        assert!(matches!(error, RpcError::Json(_)), "{error}");
        let response = error.response().unwrap();
        assert_eq!(response.id, Some(Value::Null));
        assert_eq!(response.error.as_ref().unwrap().code, PARSE_ERROR);
        // The null id is emitted, as the protocol requires.
        assert!(serde_json::to_string(&response)
            .unwrap()
            .contains("\"id\":null"));
        assert_eq!(
            read_message(&mut input).unwrap().unwrap().method.as_deref(),
            Some("exit")
        );
    }

    #[test]
    fn json_that_is_not_a_message_is_an_invalid_request() {
        for (body, id) in [
            ("[1,2]", Value::Null),
            ("42", Value::Null),
            (r#"{"id":5,"method":7}"#, json!(5)),
            (r#"{"id":"a","params":{},"error":"nope"}"#, json!("a")),
        ] {
            let error = read_message(&mut reader(&frame(body))).unwrap_err();
            assert!(matches!(error, RpcError::Invalid { .. }), "{body}: {error}");
            let response = error.response().unwrap();
            assert_eq!(response.id, Some(id), "{body}");
            assert_eq!(response.error.unwrap().code, INVALID_REQUEST, "{body}");
        }
    }

    #[test]
    fn an_end_inside_a_message_is_a_stream_error() {
        let error = read_message(&mut reader("Content-Length: 10\r\n")).unwrap_err();
        assert!(matches!(error, RpcError::Io(_)), "{error}");
        assert!(!error.is_recoverable());
        let error = read_message(&mut reader("Content-Length: 10\r\n\r\n{}")).unwrap_err();
        assert!(
            error.to_string().contains("2 bytes into a 10-byte body"),
            "{error}"
        );
        // A huge declared length costs nothing until the bytes arrive.
        let error =
            read_message(&mut reader("Content-Length: 99999999999999\r\n\r\n{}")).unwrap_err();
        assert!(matches!(error, RpcError::Io(_)), "{error}");
        // Blank lines before a message are not a message.
        assert!(read_message(&mut reader("\r\n\r\n")).unwrap().is_none());
    }

    /// A writer the test can read back.
    #[derive(Clone, Default)]
    struct Shared(std::sync::Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_connection_frames_both_ways_and_reports_bad_input_in_order() {
        let src = format!(
            "{}{}{}",
            frame(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#),
            frame("nope"),
            frame(r#"{"jsonrpc":"2.0","method":"exit"}"#)
        );
        let out = Shared::default();
        let conn = Connection::new(reader(&src), out.clone());
        let first = match conn.recv().unwrap() {
            Received::Message(message) => message,
            other => panic!("{other:?}"),
        };
        assert_eq!(first.method.as_deref(), Some("initialize"));
        assert!(matches!(conn.recv(), Err(RpcError::Json(_))));
        match conn.recv_timeout(Duration::from_secs(5)).unwrap() {
            Received::Message(message) => assert_eq!(message.method.as_deref(), Some("exit")),
            other => panic!("{other:?}"),
        }
        assert!(matches!(conn.recv().unwrap(), Received::Closed));

        conn.respond(json!(1), Value::Null).unwrap();
        conn.notify("window/logMessage", json!({"type": 3, "message": "hi"}))
            .unwrap();
        conn.respond_error(json!(2), METHOD_NOT_FOUND, "Unhandled method x")
            .unwrap();
        let written = out.0.lock().unwrap().clone();
        let mut input = std::io::Cursor::new(written);
        let response = read_message(&mut input).unwrap().unwrap();
        assert_eq!(response.result, Some(Value::Null));
        let notification = read_message(&mut input).unwrap().unwrap();
        assert_eq!(notification.method.as_deref(), Some("window/logMessage"));
        let error = read_message(&mut input).unwrap().unwrap();
        assert_eq!(error.error.unwrap().code, METHOD_NOT_FOUND);
    }

    #[test]
    fn a_quiet_connection_times_out() {
        // A reader that never yields: a pipe whose write end stays open.
        struct Stalled(std::sync::mpsc::Receiver<()>);
        impl Read for Stalled {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                let _ = self.0.recv();
                Ok(0)
            }
        }
        let (hold, stalled) = std::sync::mpsc::channel();
        let conn = Connection::new(Stalled(stalled), std::io::sink());
        assert!(matches!(
            conn.recv_timeout(Duration::from_millis(20)).unwrap(),
            Received::Timeout
        ));
        drop(hold);
        assert!(matches!(conn.recv().unwrap(), Received::Closed));
    }
}
