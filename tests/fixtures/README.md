# The spec's own example bundle

`calculator-rust.manifest.json` and `calculator-rust.mcpbignore` are the ones
checked into the MCP Bundle repository for its Rust example, unmodified:

- source: <https://github.com/modelcontextprotocol/mcpb/tree/main/examples/calculator-rust>
- fetched: 2026-11-25
- © the MCP project contributors, licensed under Apache-2.0 (the specification
  repository's licence); this crate is MIT and does not relicense them, and they
  are used only by the tests

`tests/manifest.rs` reads the manifest back into a `BundleSpec` and checks that
what the crate writes of it is the same manifest, so "we write the shape the
spec's own tooling writes" is compared against a real manifest rather than
assumed from our own tests. It also packs that spec and reads the bundle back
for the same manifest one version later.

The `.mcpbignore` is the example's own exclusion list, which the directory-tree
tests use as a real-world ignore file.

# The action's bundle

`action.manifest.json` is ours: the manifest the CI `action` job packs with the
GitHub Action, the `pack` example standing in for a server. It names every
platform, so each runner can pack it for its own.
