# TODO

What this crate is short of, and what it would take to make it worth using outside this project.
The first three sections follow the MCP Bundle spec ([MANIFEST.md][manifest], [CLI.md][cli]); the
last two are about the crate and its release.

[manifest]: https://github.com/modelcontextprotocol/mcpb/blob/main/MANIFEST.md
[cli]: https://github.com/modelcontextprotocol/mcpb/blob/main/CLI.md

## Manifest fields

- [x] `author.email` and `author.url`. `author` is an `Author` now, with the name it requires.
- [x] `long_description`, `homepage`, `documentation`, `support`, `repository`, `license`,
  `keywords`, `privacy_policies`, as fields on `BundleSpec` and keys in `BundleSpec::manifest`.
- [x] `tools_generated` and `prompts_generated`.
- [x] `prompts`: a prompt is more than the `(name, description)` pair `tools` gets away with, so it
  carries argument names and `text` with `${arguments.<name>}` placeholders, and `validate` refuses
  a prompt whose text uses an argument it does not declare.
- [x] `icons` (array of `src`, `size` as `WIDTHxHEIGHT`, `theme` for light/dark) and the legacy
  single `icon`. The files are written to the bundle's root under their own names, which is where
  the manifest points; the first icon without a theme is also written as the plain `icon`.
- [x] `screenshots`, written to the bundle's root under their own names like the icons. A nested
  path in the manifest needs the directory walk below.
- [x] `localization`: `default_locale` and the per-locale files in the bundle. `resources` is fixed
  to `mcpb-resources/${locale}.json`; a path of the caller's choosing would need a field.
- [x] `_meta` as raw JSON: `BundleSpec::meta`, a map from reverse-DNS namespace to the object the
  manifest writes, which `validate` checks is an object.
- [x] `manifest_version` stays the constant `MANIFEST_VERSION` `0.3`. 0.4 adds the `uv` server type
  and nothing a binary bundle can use; a `BundleSpec` carrying the version is for whenever a field
  shows up that only 0.4 has.

## `server.mcp_config` and `user_config`

- [x] `env`, which is where a `user_config` secret belongs: `"API_KEY": "${user_config.api_key}"`.
- [ ] One bundle per OS. `platform_overrides` is written, but only for the bundle's own platform:
  `compatibility.platforms` is `[platform]` and `BundleSpec::entry_point` derives one file name, so
  a bundle cannot hold the darwin, win32 and linux binaries and pick between them yet. Lifting that
  needs `platforms` as a list and one executable per platform. Per-CPU architecture still cannot be
  switched, so x64 and arm64 stay separate bundles unless the server is a universal macOS binary.
- [x] `user_config` entries of every kind: `string` (with `sensitive`), `number` (with `min` and
  `max`), `boolean`, `directory`, `file`, and `multiple` and `default` on all of them. `UserConfig`
  has a constructor per kind, and `validate` checks that a default fits its type. This removed
  `DirectoryPrompt` and the argument it appended: `args` now names `${user_config.<key>}` itself.

## Packaging

- [x] Validate before packing: `BundleSpec::validate` refuses a `version` that is not semver, a
  `name` that cannot be a file name, an empty description or author, variables clients do not
  substitute, duplicate tool, prompt and argument names and icon sizes that are not `WIDTHxHEIGHT`,
  and `pack` refuses such a spec before it writes anything, as well as a file it cannot read.
- [ ] An `mcpb validate` equivalent: check the written manifest against the official JSON schema at
  `schemas/mcpb-manifest-v0.3.schema.json` in the spec repo, so what clients accept is tested rather
  than assumed.
- [ ] Pack a directory tree instead of single files: `.mcpbignore` plus the CLI's default
  exclusion list (`.git`, `.DS_Store`, `node_modules/.cache`, `*.map`, lockfiles, …) and nested
  paths. `BundleSpec::files` deals in single files addressed by their own file name, placed under
  `server/` or at the root. This is also what node, python and `uv` servers need, which would mean
  supporting more than the `binary` server type — a separate decision, since the crate's point is
  that there is no runtime to ship.
- [ ] Read a bundle back (`mcpb info`): size, manifest, signature state. Useful on its own for
  checking what a release actually contains.
- [ ] `sign` / `verify` / `unsign`: a PKCS#7 SignedData trailer after the zip — the zip bytes, then
  `MCPB_SIG_V1`, a 4-byte little-endian length, the DER signature, `MCPB_SIG_END`. An unsigned
  bundle stays a plain zip, so this can come late; signing with the `mcpb` CLI after packing works
  today. A crypto stack is a heavy dependency for a crate that currently only writes files, so put
  it behind a feature. Decided against for 0.2.0: clients only treat a CA-issued certificate as
  signed, so a self-signed trailer buys little until someone needs a signed release.
- [ ] Stream the executable into the zip instead of `fs::read`ing it whole, and pack to any
  `Write + Seek` rather than only to a path.
- [ ] Pin reproducibility with a test. Entries are already written in a fixed order and the
  timestamps come out fixed, but the latter is `zip`'s default with our feature set, not a
  decision of ours; packing the same input twice should be proven to give the same bytes.

## Making it usable from other projects

- [ ] An error type instead of `String` in every `Result` — callers cannot match on a message,
  and `?` into their own error type needs a conversion.
- [x] `BundleSpec` ergonomics: `Default` and `..BundleSpec::default()`, so callers spell only what
  they set. A builder is not needed while the fields are public and the struct is plain data.
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
- [x] Crate docs as the front door: the spec version written, the platform limitation, and an
  example that is a doctest. The README is the crate's documentation (`#![doc = include_str!]`), so
  `cargo test` compiles its example.
- [ ] CI: a matrix job that cross-compiles a server per target and packs a bundle per target. That
  is the question every user of this crate has, and a working answer is better than a paragraph.
  (CI itself exists: fmt, clippy, docs and the tests on three platforms.)
- [ ] Prove the declared MSRV: `rust-version = "1.85"`, which edition 2024 needs. CI tests the
  stable toolchain alone, and an MSRV job needs a committed `Cargo.lock`, since dependencies float
  while the lockfile is ignored.
- [ ] Test against the spec's own examples, at least
  [`examples/calculator-rust`](https://github.com/modelcontextprotocol/mcpb/tree/main/examples/calculator-rust):
  read the bundle the Node CLI produced and compare its manifest to ours, so "same shape" is
  checked rather than assumed.

## Publishing

- [x] `mcpb` is reserved on crates.io with 0.1.0.
- [ ] 0.1.0 cannot be replaced, only yanked, so treat the API as unstable: breaking changes bump
  the minor version.
- [x] Publish from a tag in CI instead of from a working copy, so the release and the commit always
  match. `.github/workflows/release.yml` publishes on a `v<version>` tag through crates.io Trusted
  Publishing and creates the GitHub release from the changelog.
- [ ] Allow that workflow to publish: crates.io → the crate → Settings → Trusted Publishing → add a
  GitHub publisher for owner `Synphonyte`, repository `mcpb`, workflow `release.yml`.
- [ ] Cut 0.2.0: date the changelog's `0.2.0` section, commit, tag `v0.2.0`, push, and watch the
  Release run publish the crate and open the release.
- [ ] Check the built docs.rs page once the first release lands.
