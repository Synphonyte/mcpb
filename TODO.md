# TODO

What this crate is short of, and what it would take to make it worth using outside this project.
The first three sections follow the MCP Bundle spec ([MANIFEST.md][manifest], [CLI.md][cli]); the
last two are about the crate and its release.

[manifest]: https://github.com/modelcontextprotocol/mcpb/blob/main/MANIFEST.md
[cli]: https://github.com/modelcontextprotocol/mcpb/blob/main/CLI.md

## Manifest fields

- [ ] `author.email` and `author.url`.
- [ ] `long_description`, `homepage`, `documentation`, `support`, `repository`, `license`,
  `keywords`, `privacy_policies`. Nothing new to model here: fields on `BundleSpec` and keys in
  `BundleSpec::manifest`.
- [ ] `tools_generated`, and `prompts` / `prompts_generated`. A prompt is more than the
  `(name, description)` pair `tools` gets away with: it carries argument names and `text` with
  `${arguments.<name>}` placeholders.
- [ ] `icons` (array of `src`, `size` as `WIDTHxHEIGHT`, `theme` for light/dark) and the legacy
  single `icon`, plus `screenshots`. The files sit in the bundle at the path the manifest names,
  which is not under `server/` and is not something `pack` can write today — see *Packaging*.
- [ ] `localization`: `resources` (must contain a `${locale}` placeholder) and `default_locale`,
  together with the per-locale `mcpb-resources/<locale>.json` files in the bundle.
- [ ] `_meta`, at least as raw JSON to insert verbatim: Windows `package_family_name` and static
  responses, macOS bundle identifiers. Bundles for those stores cannot be built without it.
- [ ] `manifest_version` is the constant `MANIFEST_VERSION` `0.3`, so a 0.4 manifest (the `uv`
  server type and whatever else 0.4 adds) cannot be written at all. Decide whether
  `BundleSpec` carries the version it writes.

## `server.mcp_config` and `user_config`

- [ ] `env`, which is where a `user_config` secret belongs: `"API_KEY": "${user_config.api_key}"`.
- [ ] `platform_overrides` (per platform: `command`, `args`, `env`). This is what lifts "one
  bundle per OS": one bundle can hold the darwin, win32 and linux binaries and pick between them.
  Per-CPU architecture still cannot be switched, so x64 and arm64 stay separate bundles unless the
  server is a universal macOS binary.
- [ ] `user_config` entries other than a `directory`: `string` with `sensitive`, `number` with
  `min`/`max`, `boolean`, `file`, and `multiple` / `default` on all of them. `DirectoryPrompt`
  covers one fixed case; the general thing is a struct per entry, with the directory prompt as a
  preset of it.

## Packaging

- [ ] Validate before packing, and an `mcpb validate` equivalent. `pack` happily writes a bundle
  no client accepts: a `version` that is not semver, an `entry_point` with no file behind it, a
  `name` with characters clients reject. The official JSON schema is at
  `schemas/mcpb-manifest-v0.3.schema.json` in the spec repo.
- [ ] Pack a directory tree instead of single files: `.mcpbignore` plus the CLI's default
  exclusion list (`.git`, `.DS_Store`, `node_modules/.cache`, `*.map`, lockfiles, …), nested
  paths, and paths in the bundle other than `server/` so icons and assets can be placed.
  `BundleSpec::files` deals in single files addressed by their own file name. This is also what
  node, python and `uv` servers need, which would mean supporting more than the `binary` server
  type — a separate decision, since the crate's point is that there is no runtime to ship.
- [ ] Read a bundle back (`mcpb info`): size, manifest, signature state. Useful on its own for
  checking what a release actually contains.
- [ ] `sign` / `verify` / `unsign`: a PKCS#7 SignedData trailer after the zip — the zip bytes, then
  `MCPB_SIG_V1`, a 4-byte little-endian length, the DER signature, `MCPB_SIG_END`. An unsigned
  bundle stays a plain zip, so this can come late. A crypto stack is a heavy dependency for a
  crate that currently only writes files, so put it behind a feature.
- [ ] Stream the executable into the zip instead of `fs::read`ing it whole, and pack to any
  `Write + Seek` rather than only to a path.
- [ ] Pin reproducibility with a test. Entries are already written in a fixed order and the
  timestamps come out fixed, but the latter is `zip`'s default with our feature set, not a
  decision of ours; packing the same input twice should be proven to give the same bytes.

## Making it usable from other projects

- [ ] An error type instead of `String` in every `Result` — callers cannot match on a message,
  and `?` into their own error type needs a conversion.
- [ ] `BundleSpec` ergonomics: `Default`, or a builder, so callers do not spell every field. The
  struct will only grow as the manifest fields above land.
- [ ] Typed where the spec constrains the string: `version` should be a semver type, and
  `BundleSpec::file_name` takes `arch: &str` that nothing checks. `Platform::parse` accepts
  `macos` and `windows` while `Platform::as_str` writes the spec's spelling — decide whether that
  leniency stays a convenience or becomes an error.
- [ ] `Serialize` / `Deserialize` for `BundleSpec` and `Platform`, so a manifest can be read,
  edited and written back. `validate`, `info` and "same manifest, new version" all need it, and it
  lets a caller keep the manifest in its own config file.
- [ ] `Bundle::open`: read the manifest of an existing `.mcpb`, and extract it.
- [ ] A binary, so this is `cargo install mcpb` for people who do not want Node installed: `pack`,
  `info`, `validate`.
- [ ] Crate docs as the front door: the spec version written, the platform limitation, and an
  example that is a doctest (the README example is not compiled, so it can rot).
- [ ] CI: a matrix job that cross-compiles a server per target and packs a bundle per target. That
  is the question every user of this crate has, and a working answer is better than a paragraph.
- [ ] Test against the spec's own examples, at least
  [`examples/calculator-rust`](https://github.com/modelcontextprotocol/mcpb/tree/main/examples/calculator-rust):
  read the bundle the Node CLI produced and compare its manifest to ours, so "same shape" is
  checked rather than assumed.

## Publishing

- [x] `mcpb` is reserved on crates.io with 0.1.0.
- [ ] 0.1.0 cannot be replaced, only yanked, so treat the API as unstable: breaking changes bump
  the minor version.
- [ ] Publish from a tag in CI instead of from a working copy, so the release and the commit always
  match.
- [ ] Check the built docs.rs page once the first release lands.
