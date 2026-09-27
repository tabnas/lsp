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
//! Status: the message types, the error type and the response helpers
//! are complete; framing ([`read_message`], [`write_message`]) and the
//! connection are signatures for the server module agent.

use std::fmt;
use std::io::{BufRead, Read, Write};
use std::sync::mpsc::Receiver;
use std::sync::Mutex;
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    #[serde(default = "version")]
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
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

/// A transport failure: the stream, a malformed frame, or a body that
/// is not JSON.
#[derive(Debug)]
pub enum RpcError {
    Io(std::io::Error),
    Frame(String),
    Json(serde_json::Error),
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RpcError::Io(error) => write!(f, "jsonrpc: {error}"),
            RpcError::Frame(message) => write!(f, "jsonrpc: {message}"),
            RpcError::Json(error) => write!(f, "jsonrpc: bad JSON-RPC body: {error}"),
        }
    }
}

impl std::error::Error for RpcError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RpcError::Io(error) => Some(error),
            RpcError::Frame(_) => None,
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
/// exactly that many bytes of body. `Ok(None)` at a clean end of stream.
#[allow(unused_variables)] // stub
pub fn read_message(reader: &mut dyn BufRead) -> Result<Option<Message>, RpcError> {
    todo!("jsonrpc::read_message: go/jsonrpc.go read")
}

/// Write one framed message: `Content-Length: <n>\r\n\r\n` and the body,
/// then flush.
#[allow(unused_variables)] // stub
pub fn write_message(writer: &mut dyn Write, message: &Message) -> Result<(), RpcError> {
    todo!("jsonrpc::write_message: go/jsonrpc.go writeJSON")
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
    #[allow(unused_variables)] // stub
    pub fn new(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
    ) -> Connection {
        todo!("jsonrpc::Connection::new: the reader thread over read_message")
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
}
