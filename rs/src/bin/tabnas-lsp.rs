// Copyright (c) 2026 Richard Rodger, MIT License

//! The `tabnas-lsp` command: the unified language server over stdio.
//! The implementation is the `tabnas_lsp` crate; this is the launcher,
//! the port of `ts/bin/tabnas-lsp.js`. It builds the server's
//! configuration from the bundled registry and the binary's loader (the
//! fleet grammars when linked in, L2 specs and, with `dialects`, L3
//! grammar files from workspace configuration) and runs a
//! [`tabnas_lsp::Server`] over standard input and output.
//!
//! The exit status is the protocol's, as `vscode-languageserver` sets
//! it: 0 when the client sent `shutdown` before `exit` (or before
//! closing the stream), 1 otherwise, and 1 when the stream fails; 2 for
//! a command line this launcher does not take.

use std::io::Write;
use std::process::ExitCode;

use tabnas_lsp::jsonrpc::Connection;
use tabnas_lsp::loaders::Loader;
use tabnas_lsp::registry::Registry;
use tabnas_lsp::{Config, Entry, Server};

const USAGE: &str = "\
usage: tabnas-lsp --stdio [--clientProcessId=<pid>]
       tabnas-lsp --version
       tabnas-lsp --help

The unified tabnas language server: one process serving every
registered tabnas grammar, routing per document by languageId and
extension, over JSON-RPC on standard input and output.

Grammars are added at runtime through initializationOptions.languages
or a workspace folder's .tabnas/lsp.json: {\"languages\": [{\"languageId\":
\"mydsl\", \"extensions\": [\".mydsl\"], \"load\": {\"grammar\":
\"./grammar/mydsl.abnf\"}}]}. A `spec` load names a serialized
GrammarSpec; a `grammar` load names .abnf, .ebnf or .gbnf text (needs
the `dialects` feature); a `module` load names a grammar linked into
this binary (the `fleet` feature), and a workspace entry may load one
only when initializationOptions.trustWorkspaceModules is true.";

fn main() -> ExitCode {
    // Arguments after the program name, as `process.argv.slice(2)` gives
    // the canonical command.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    match args[..] {
        ["--version"] | ["-V"] => {
            println!("{}", version_line());
            ExitCode::SUCCESS
        }
        ["--help"] | ["-h"] => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        _ if is_stdio(&args) => stdio(),
        _ => {
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(stderr, "{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// `--stdio`, alone or with the `--clientProcessId` a language client
/// may add (`--clientProcessId=<pid>` or `--clientProcessId <pid>`). The
/// process id is accepted and not watched: the server ends when the
/// client closes its standard input, which a client that dies does.
fn is_stdio(args: &[&str]) -> bool {
    let mut stdio = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match *arg {
            "--stdio" if !stdio => stdio = true,
            "--clientProcessId" => {
                if !rest.next().is_some_and(|pid| is_pid(pid)) {
                    return false;
                }
            }
            other => match other.strip_prefix("--clientProcessId=") {
                Some(pid) if is_pid(pid) => {}
                _ => return false,
            },
        }
    }
    stdio
}

fn is_pid(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// The version line: this crate, the engine, and what is built in.
fn version_line() -> String {
    let mut features = Vec::new();
    if cfg!(feature = "fleet") {
        features.push("fleet");
    }
    if cfg!(feature = "dialects") {
        features.push("dialects");
    }
    let built = if features.is_empty() {
        "no optional features".to_string()
    } else {
        format!("features: {}", features.join(", "))
    };
    format!(
        "tabnas-lsp {} (tabnas engine {}; {built})",
        tabnas_lsp::VERSION,
        tabnas::VERSION
    )
}

/// The binary's loader: the fleet grammars when linked in, else an
/// empty one that serves configured L2 and L3 entries only.
fn loader() -> Loader {
    #[cfg(feature = "fleet")]
    {
        tabnas_lsp::loaders::fleet()
    }
    #[cfg(not(feature = "fleet"))]
    {
        Loader::new()
    }
}

/// The bundled tier: the registry's entries whose grammar this binary
/// can build, which is every entry with a linked module (no bundled
/// entry names a spec or grammar file). The canonical server keeps every
/// entry and fails at `require` time for a package that is not
/// installed; a Rust binary knows at build time what it links, so an
/// entry it cannot build is left out rather than routed to a load that
/// can only fail.
fn bundled_entries(loader: &Loader) -> Vec<Entry> {
    Registry::bundled()
        .entries
        .iter()
        .filter(|entry| loader.linked(&entry.name).is_some())
        .cloned()
        .collect()
}

fn stdio() -> ExitCode {
    let loader = loader();
    // One trust flag for both gates: the loader's own, and the server's
    // around it, which `initializationOptions.trustWorkspaceModules`
    // sets.
    let trust = loader.trust_handle();
    let mut config = Config::new(loader.clone().into_make_instance());
    config.entries = bundled_entries(&loader);
    let mut server = Server::with_trust(config, Connection::stdio(), trust);
    match server.run() {
        Ok(()) if server.shutdown_requested() => ExitCode::SUCCESS,
        Ok(()) => ExitCode::FAILURE,
        Err(error) => {
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(stderr, "tabnas-lsp: {error}");
            ExitCode::FAILURE
        }
    }
}
