# Vendored manifest schema

`mcpb-manifest-v0.3.schema.json` is the JSON schema of the MCP Bundle manifest
format, taken unmodified from the specification repository:

- source: <https://github.com/modelcontextprotocol/mcpb/blob/main/schemas/mcpb-manifest-v0.3.schema.json>
- fetched: 2026-11-24, manifest version 0.3
- © the MCP project contributors, licensed under Apache-2.0 (the
  specification repository's licence); this crate is MIT and does not relicense
  it, and it is used only by the test that checks the manifests we write

`tests/manifest.rs` validates the manifests this crate produces against it, so
that "clients accept this manifest" is not only what our own tests expect. The
file is not compiled into the crate; `include_str!` in that test reads it.

When the format moves on, replace the file with the new version's schema, keep
the name in `manifest.rs`/`Cargo.toml` in step, and fix what the schema reports
until it passes again.
