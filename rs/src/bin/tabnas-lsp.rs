// Copyright (c) 2026 Richard Rodger, MIT License

//! The `tabnas-lsp` command: the unified language server over stdio.
//! The implementation is the `tabnas_lsp` crate; this is the launcher,
//! the port of `ts/bin/tabnas-lsp.js`. It builds the server's
//! configuration from the bundled registry and the binary's loader (the
//! fleet grammars when linked in, L2 specs and, with `dialects`, L3
//! grammar files from workspace configuration) and hands it to
//! `tabnas_lsp::server::serve`.
//!
//! Status: usage and `--version` are complete; `--stdio` builds the
//! configuration and calls the server, which is a stub until the server
//! module lands.

use std::io::Write;
use std::process::ExitCode;

use tabnas_lsp::loaders::Loader;
use tabnas_lsp::registry::Registry;
use tabnas_lsp::{Config, Entry};

const USAGE: &str = "\
usage: tabnas-lsp --stdio
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
this binary (the `fleet` feature).";

fn main() -> ExitCode {
    // Arguments after the program name, as `process.argv.slice(2)` gives
    // the canonical command.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match argv.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["--stdio"] => stdio(),
        ["--version"] | ["-V"] => {
            println!("{}", version_line());
            ExitCode::SUCCESS
        }
        ["--help"] | ["-h"] => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        _ => {
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(stderr, "{USAGE}");
            ExitCode::from(2)
        }
    }
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
/// can build, which is every entry with a linked module.
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
    let mut config = Config::new(loader.clone().into_make_instance());
    config.entries = bundled_entries(&loader);
    match tabnas_lsp::server::serve(config) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(stderr, "tabnas-lsp: {error}");
            ExitCode::FAILURE
        }
    }
}
