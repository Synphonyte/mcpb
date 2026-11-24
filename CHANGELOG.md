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
- `BundleSpec::env`, written as `server.mcp_config.env`.
- `BundleSpec::platform_overrides`, written as
  `server.mcp_config.platform_overrides` (`PlatformConfig`: `command`, `args`,
  `env`) for the bundle's own platform; an override for another platform is
  refused, since a bundle runs on one platform.
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

- **Breaking:** `BundleSpec` gained the fields `icon`, `icons`, `env`,
  `platform_overrides`, `prompts` and `localization`, so every spec literal
  needs them.
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
