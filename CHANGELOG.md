# Changelog

All notable changes to this crate are documented here. It follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 0.2.0 - unreleased

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
- `BundleSpec::pack_to` writes the bundle to any `Write + Seek`, such as a `Cursor` over a `Vec`,
  and hands it back; `pack` writes it to a path through the same code.
- `pack`, `pack_to` and everything they call return `Error`, an enum that implements `Debug`,
  `Display` and `std::error::Error` (`Invalid`, `Read`, `Write`, `Name`, `Icon`, `Clash`) instead of
  a `String`, so callers can match on what went wrong and `?` it into their own error type - anyhow
  included.
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
- Crate metadata for publishing: `repository`, `readme`, `keywords` and
  `rust-version`.
- The README is now the crate documentation, so its example is compiled as a
  doctest.

### Changed

- **Breaking:** `BundleSpec` has the new fields `long_description`, `repository`,
  `homepage`, `documentation`, `support`, `license`, `keywords`,
  `privacy_policies`, `screenshots`, `tools_generated`, `prompts_generated`,
  `meta`, `icon`, `icons`, `env`, `platform_overrides`, `prompts` and
  `localization`, and `author` is an `Author` rather than a `String`, so every
  spec literal needs updating - or `..BundleSpec::default()`.
- **Breaking:** `directory` is gone in favour of `UserConfig::directory` in
  `user_config`, and `pack` no longer appends it as an argument: `args` names
  `${user_config.<key>}` where the spec wants it.
- **Breaking:** `platform` and `executable` are gone in favour of
  `executables`, a map from `Platform` to `Executable` (`path` and `arch`),
  which is how a bundle runs on several platforms. `file_name` takes no
  argument now - it names every platform with its architecture, such as
  `<name>-<version>-darwin-arm64-win32-x64.mcpb` - and `entry_point` returns
  `Option<String>`, the first platform's binary.
- `validate` also refuses an author without a name, an email or a URL that does
  not look like one, an empty `long_description` or `license`, repeated
  keywords, a `_meta` namespace that holds no object, and a `user_config` entry
  whose key, default, or `min` and `max` do not fit its type.
- `validate` refuses a bundle without an executable, an architecture that cannot
  be a file name, and a `platform_overrides` entry for a platform the bundle has
  no executable for.
- The same spec now packs to the same bytes every time: the entries are written in a fixed order
  with a fixed timestamp, and the executable is streamed into the zip instead of read whole.
- `pack` refuses a spec that `validate` rejects, and an icon that is not a PNG,
  before it writes anything.
- A bundle file may no longer be named `manifest.json`, which used to clobber
  the manifest in the archive.
- Files are written under their own file names only when those are UTF-8;
  before, other names were converted lossily.

### Dependencies

- Added `semver`, which checks the version of a bundle.

## 0.1.0 - 2026-11-20

### Added

- First release: `BundleSpec` with `manifest`, `pack` and `file_name`,
  `Platform`, and the directory prompt clients show at install.
