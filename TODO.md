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
- [x] One bundle per OS: `executables` maps each `Platform` to an `Executable` (path and arch), the
  bundle declares every platform in `compatibility.platforms`, writes each binary under its own name
  (`<name>`, `<name>-<platform>`, `.exe` on Windows) and generates the `platform_overrides` command
  that starts each platform's binary. `file_name` names all of them with their architectures.
  Per-CPU architecture still cannot be switched, so x64 and arm64 stay separate bundles unless the
  server is a universal macOS binary.
- [x] `user_config` entries of every kind: `string` (with `sensitive`), `number` (with `min` and
  `max`), `boolean`, `directory`, `file`, and `multiple` and `default` on all of them. `UserConfig`
  has a constructor per kind, and `validate` checks that a default fits its type. This removed
  `DirectoryPrompt` and the argument it appended: `args` now names `${user_config.<key>}` itself.

## Packaging

- [x] Validate before packing: `BundleSpec::validate` refuses a `version` that is not semver, a
  `name` that cannot be a file name, an empty description or author, variables clients do not
  substitute, duplicate tool, prompt and argument names and icon sizes that are not `WIDTHxHEIGHT`,
  and `pack` refuses such a spec before it writes anything, as well as a file it cannot read.
- [x] An `mcpb validate` equivalent: `cargo test` checks the manifests the crate writes against the
  official JSON schema (`tests/manifest.rs`), with the schema vendored in `tests/schemas/` and
  `jsonschema` as a dev-dependency. The CI job that packs a bundle per platform also runs the real
  `mcpb validate` over the artifact.
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
- [x] Stream the executable into the zip instead of `fs::read`ing it whole, and pack to any
  `Write + Seek` rather than only to a path: `BundleSpec::pack_to` takes one, and `pack` is the file
  and rename around it.
- [x] Pin reproducibility with a test. The entries are written in a fixed order with a fixed
  timestamp (`pack_to` sets it rather than leaving it to `zip`'s default), and a test packs the same
  spec twice and compares the bytes, and the file with the in-memory bundle.

## Making it usable from other projects

- [x] An error type instead of `String` in every `Result`: `Error` implements `Debug`, `Display`
  and `std::error::Error` with a variant per kind of failure (`Invalid` for the `validate`
  problems, `Read`, `Write`, `Name`, `Icon`, `Clash`), so callers can match and `?` it into their
  own error type. `validate` still returns the problems as `Vec<String>`, since that is a report.
- [x] `BundleSpec` ergonomics: `Default` and `..BundleSpec::default()`, so callers spell only what
  they set. A builder is not needed while the fields are public and the struct is plain data.
- [x] Typed where the spec constrains the string: `version` is a `semver::Version` (re-exported as
  `mcpb::Version`), `Executable` carries an `Option<String>` arch that `validate` checks, and
  `Platform::parse` takes only the manifest's spelling (`darwin`, `linux`, `win32`), so `macos` is
  reported rather than packed.
- [x] `Serialize` / `Deserialize` for `BundleSpec` and `Platform`, so a manifest can be read,
  edited and written back: `BundleSpec::from_manifest`, `in_dir` and the manifest's own shape as
  serde form, which is what "same manifest, new version" and the reference-manifest test use.
- [x] `Bundle::open`: read the manifest of an existing `.mcpb`, list what it holds, extract it, and
  read its spec back with `Bundle::spec`.
- [ ] A binary, so this is `cargo install mcpb` for people who do not want Node installed: `pack`,
  `info`, `validate`.
- [x] Crate docs as the front door: the spec version written, the platform limitation, and an
  example that is a doctest. The README is the crate's documentation (`#![doc = include_str!]`), so
  `cargo test` compiles its example.
- [x] CI packs a bundle: `examples/pack.rs` packs the example binary (or one you point it at, with `--executable` and `--arch`), and a job on each of the three platforms unpacks the result and has the official CLI validate the manifest. On macOS it also packs the other Apple architecture from a cross-built binary. Arbitrary cross-compilation (Linux arm64, Windows arm64) would need other toolchains and is not there.
- [ ] Prove the declared MSRV: `rust-version = "1.85"`, which edition 2024 needs. CI tests the
  stable toolchain alone, and an MSRV job needs a committed `Cargo.lock`, since dependencies float
  while the lockfile is ignored.
- [x] Test against the spec's own examples, at least
  [`examples/calculator-rust`](https://github.com/modelcontextprotocol/mcpb/tree/main/examples/calculator-rust):
  its manifest is vendored in `tests/fixtures/`, and the tests check that the crate reads it,
  writes the same manifest, packs it, and reads the same manifest back from the packed bundle.

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
