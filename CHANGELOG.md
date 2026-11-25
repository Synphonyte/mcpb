# Changelog

All notable changes to this crate are documented here. It follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added

- A screenshot whose path is also one of `BundleSpec::files` is written, and
  named in the manifest, where that `BundleFile` puts it, such as
  `assets/shot.png`; a manifest's nested screenshots read back that way.
- Icons are placed the same way: an `icon` or `icons` entry whose path is also
  one of `BundleSpec::files` is written, and named in the manifest (`icon` and
  `icons[].src`), where that file puts it; a manifest's nested icons read back
  that way, an icon named twice placed once.
- CI tests and packs on the `ubuntu-24.04-arm` and `windows-11-arm` runners, so
  Linux and Windows arm64 are covered.

## 0.2.0 - 2026-11-24

### Added

- `BundleSpec::validate`, which reports everything clients would refuse in a
  spec: names that cannot be file names, versions that are not semantic
  versions, empty descriptions, `${...}` variables clients do not substitute,
  environment variable names, icon sizes, locale codes, and tool, prompt and
  argument names that are used twice.
- `BundleSpec::icon` and `BundleSpec::icons`: PNG icons, written to the root of
  the bundle. The first icon without a theme is also written as the manifest's
  plain `icon`, and one file used as both is written once.
- `BundleSpec::screenshots`, written to the root of the bundle next to the icons.
- `Author` (`name`, `email`, `url`) and `Repository` (`kind`, `url`), and the
  metadata stores show: `long_description`, `homepage`, `documentation`,
  `support`, `license`, `keywords` and `privacy_policies`.
- `BundleSpec::tools_generated`, `BundleSpec::prompts_generated` and
  `BundleSpec::meta`, which writes `_meta` verbatim from a map of reverse-DNS
  namespaces to JSON objects.
- `BundleSpec::user_config` (`UserConfig`: `key`, `kind`, `title`, `description`, `required`,
  `sensitive`, `multiple`, `default`, `min`, `max`), with a constructor per `UserConfigKind`
  (`string`, `number`, `boolean`, `directory`, `file`), written as the manifest's `user_config`.
- `BundleSpec::files` (`BundleFile`: `path`, `name`, `mode`), one file written
  under a name of its own anywhere in the bundle.
- `BundleSpec::pack_to` writes the bundle to any `Write + Seek`, such as a `Cursor` over a `Vec`,
  and hands it back; `pack` writes it to a path through the same code.
- `pack`, `pack_to` and everything they call return `Error`, an enum that implements `Debug`,
  `Display` and `std::error::Error` (`Invalid`, `Version`, `Read`, `Write`, `Name`, `Icon`, `Clash`,
  `Bundle`, `Unsafe`) instead of a `String`, so callers can match on what went wrong and `?` it into
  their own error type - anyhow included.
- `BundleSpec::executables`: one binary per platform the bundle runs on. A
  bundle of several platforms writes each binary (`<name>`, `<name>-<platform>`
  and `.exe` on Windows), declares every platform in
  `compatibility.platforms`, and gives each platform but the first its own
  `command` through `platform_overrides`. `Executable` carries the binary's
  `path` and `arch`.
- `Default for BundleSpec`, so a spec spells only the fields it sets.
- `BundleSpec::env`, written as `server.mcp_config.env`.
- `BundleSpec::platform_overrides`, written as
  `server.mcp_config.platform_overrides` (`PlatformConfig`: `command`, `args`,
  `env`); an override for a platform the bundle has no executable for is
  refused.
- `BundleSpec::prompts` (`Prompt`: `name`, `description`, `arguments`, `text`),
  written as the manifest's `prompts`.
- `BundleSpec::localization` (`Localization`: `default_locale`, `locales`),
  writing the manifest's `localization` and packing each locale at
  `mcpb-resources/<locale>.json`.
- `BundleSpec::version` is a [`semver::Version`] rather than a `String`, re-exported as
  `mcpb::Version`, and `Error` gained `From<semver::Error>`, so a spec is built with
  `version: "1.2.3".parse()?` and cannot hold a version semver refuses.
- `Executable::named`, which writes the binary under a name of its own (`server/mcp-calculator`) for a
  manifest that does not name its entry point after the bundle, and `Executable::arch` is
  `Option<String>`: a spec read from a manifest knows no architecture, and `file_name` then names the
  platform alone.
- `BundleSpec::platform_overrides` is only generated for a platform whose binary is a different file
  than `entry_point`, so a manifest that names one binary for several platforms stays as it is.
- `Bundle::open` reads an existing `.mcpb` back - whatever packed it - for
  `manifest`, `manifest_version`, `entry_point`, the `entries` with their sizes
  and whether they are written as programs, the file's `size`, and `is_signed`
  for the signature a bundle may carry after its zip; `extract_to` unpacks it,
  refusing a name that would leave the directory. A file that is no zip, or a
  zip without a `manifest.json`, is refused as `Error::Bundle`.
- A binary, `cargo install mcpb`: `pack` a manifest (with `--arch`, `--tree` and
  `--data`), `validate` a manifest, a directory or a bundle, and `info` a bundle -
  the same checks and reads the library offers, for people without a Rust program
  in the way.
- `BundleSpec::trees` (`Tree`): a directory packed as it is laid out, into
  `server/` or anywhere else in the bundle, minus the format's own exclusions
  and the lines of a `.mcpbignore` at its root (the `.gitignore` subset:
  comments, `!`, `*`, `?`, `**`, a trailing `/` for a directory, a pattern
  without a slash matching at any depth). `Tree::new` writes its files as
  programs and `Tree::data` as files to read; `.mcpbignore` itself is left out.
- `BundleSpec::from_manifest` reads any manifest into a spec and
  `BundleSpec::in_dir` joins the paths it names onto a directory, so a bundle
  can be read, edited and written again; `BundleSpec` is `Serialize` and
  `Deserialize` in the manifest's shape, and `Platform` in the format's
  spelling. `Bundle::spec` reads the spec out of a packed bundle, filling in what
  a manifest does not name (the architecture, the helpers and resources next to
  the server, the locales) from the files themselves.
- Tests against the manifest of the spec's own `examples/calculator-rust`,
  vendored in `tests/fixtures/`: the crate reads it, writes the same manifest,
  packs it, and reads the same manifest back one version later.
- A test that checks every shape of manifest this crate writes against the
  manifest format's own JSON schema, vendored in `tests/schemas/`, so what
  clients accept is not only what our own tests expect.
- Crate metadata for publishing: `repository`, `readme`, `keywords` and
  `rust-version`.
- The README is now the crate documentation, so its example is compiled as a
  doctest.

### Changed

- **Migrating:** a spec now reads `Author::new("…")`, `Executable::new(path, arch)`
  (`.named("…")` if the binary is not named after the bundle), `UserConfig::directory("key", …)`
  in `user_config`, `version: "1.2.3".parse()?`, and `?` on `mcpb::Error`;
  `..BundleSpec::default()` covers the rest.
- **Breaking:** `BundleSpec` has the new fields `long_description`, `repository`,
  `homepage`, `documentation`, `support`, `license`, `keywords`,
  `privacy_policies`, `screenshots`, `tools_generated`, `prompts_generated`,
  `meta`, `icon`, `icons`, `env`, `platform_overrides`, `prompts` and
  `localization`, and `author` is an `Author` rather than a `String`, so every
  spec literal needs updating - or `..BundleSpec::default()`.
- **Breaking:** `directory` is gone in favour of `UserConfig::directory` in
  `user_config`, and `pack` no longer appends it as an argument: `args` names
  `${user_config.<key>}` where the spec wants it.
- **Breaking:** `Platform::parse` takes only the manifest's spelling (`darwin`, `linux`, `win32`);
  `macos` and `windows` are no longer accepted, so a wrong platform name is reported instead of
  silently packed.
- **Breaking:** `platform` and `executable` are gone in favour of
  `executables`, a map from `Platform` to `Executable` (`path` and `arch`),
  which is how a bundle runs on several platforms. `file_name` takes no
  argument now - it names every platform with its architecture, such as
  `<name>-<version>-darwin-arm64-win32-x64.mcpb` - and `entry_point` returns
  `Option<String>`, the first platform's binary.
- `Cargo.lock` is committed: the crate has a binary, and the CI job that tests
  the declared MSRV (`rust-version = 1.85`) needs the versions that were tested
  rather than whatever a dependency released today. Every CI job runs `--locked`.
- The files of a bundle are sorted by their name in it, so the same spec packs
  the same bytes whatever order a packed directory listed them in, and neither
  `pack` nor `pack_to` packs the bundle it is writing.
- `validate` refuses a tree that goes into a path that is not inside the bundle.
- `validate` also refuses an author without a name, an email or a URL that does
  not look like one, an empty `long_description` or `license`, repeated
  keywords, a `_meta` namespace that holds no object, and a `user_config` entry
  whose key, default, or `min` and `max` do not fit its type. A version that is
  not a semantic version is no longer reported, since the type refuses it first.
- `validate` refuses a bundle without an executable, an architecture or an executable name that
  cannot be a file name, two platforms whose binaries would be written under one name, and a
  `platform_overrides` entry for a platform the bundle has no executable for.
- The same spec now packs to the same bytes every time: the entries are written in a fixed order
  with a fixed timestamp, and the executable is streamed into the zip instead of read whole.
- `pack` refuses a spec that `validate` rejects, and an icon that is not a PNG,
  before it writes anything.
- A bundle file may no longer be named `manifest.json`, which used to clobber
  the manifest in the archive.
- Files are written under their own file names only when those are UTF-8;
  before, other names were converted lossily.

- A tree packs a directory and a symlink to it under both names; only a link
  back into a directory the walk is inside is cut. A link to nothing is refused
  unless the `.mcpbignore` or the format leaves it out.
- A `.mcpbignore` pattern that starts with `/` matches from the tree's root only,
  as in a `.gitignore`.
- `user_config` is written, and read back, in the order the spec gives it, which
  is the order clients ask in.
- `from_manifest` refuses a `server.type` other than `binary` and a
  `mcp_config.command` that does not start the entry point, instead of
  rewriting them; an entry point in a subdirectory of `server/` keeps its place
  (`Executable::name` may hold `/`), and one outside `server/` is refused.
- `Bundle::spec` gives back files deeper than `server/`, or elsewhere in the
  bundle, as `BundleFile`s under their names, so a bundle read back and packed
  again keeps its layout.
- `mcpb validate` says what is not valid instead of that it "can't be packed",
  and for a bundle also reports the files its manifest names that it does not
  hold.
- A tree may go into a path with a leading or trailing `/`.

### Dependencies

- `serde_json` is built with `preserve_order`, so a manifest keeps the order it
  was written in.
- Added `semver`, which types the version of a bundle, and `serde`, whose traits
  let a manifest be serialized and deserialized in its own shape.
- Added `jsonschema`, a dev-dependency of the schema test only.

## 0.1.0 - 2026-11-20

### Added

- First release: `BundleSpec` with `manifest`, `pack` and `file_name`,
  `Platform`, and the directory prompt clients show at install.
