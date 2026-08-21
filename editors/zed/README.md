# Zed extension scaffold

Zed language extensions declare languages in TOML but bind
language servers through a small Rust shim (`src/lib.rs`
implementing `zed_extension_api`), and highlighting needs a
tree-sitter grammar reference. This scaffold carries the
declarative half; wire the shim to launch `tabnas-lsp --stdio`.
See https://zed.dev/docs/extensions/languages
