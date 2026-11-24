# mcpb

[![CI](https://github.com/Synphonyte/mcpb/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Synphonyte/mcpb/actions/workflows/ci.yml)

Pack a local MCP server as an [MCP Bundle](https://github.com/modelcontextprotocol/mcpb) (`.mcpb`): a
zip with a `manifest.json` and the server binary, which clients such as Claude Desktop install with
one click. No Node and no `mcpb` CLI.

```sh
cargo add mcpb
```

```rust,no_run
use std::collections::BTreeMap;

use mcpb::{Author, BundleSpec, Icon, Platform, Prompt, UserConfig};

fn main() -> Result<(), mcpb::Error> {
    let spec = BundleSpec {
        name: "my-server".into(),
        display_name: None,
        version: "1.2.3".into(),
        description: "What the server does.".into(),
        author: Author::new("Me"),
        platform: Platform::Darwin,
        executable: "target/release/my-server".into(),
        extra_executables: Vec::new(),
        resources: vec!["THIRD-PARTY-NOTICES.md".into()],
        icon: Some("assets/icon.png".into()),
        icons: vec![Icon {
            path: "assets/icon-dark.png".into(),
            size: "512x512".into(),
            theme: Some("dark".into()),
        }],
        tools: vec![("whoami".into(), "Who the caller is.".into())],
        prompts: vec![Prompt {
            name: "review".into(),
            description: Some("Review a file.".into()),
            arguments: vec!["path".into()],
            text: "Please review ${arguments.path}.".into(),
        }],
        localization: None,
        user_config: vec![UserConfig {
            required: true,
            ..UserConfig::directory(
                "project_dir",
                "Project directory",
                "The directory the server works in.",
            )
        }],
        args: vec!["--verbose".into(), "${user_config.project_dir}".into()],
        env: BTreeMap::from([("LOG_LEVEL".into(), "debug".into())]),
        platform_overrides: BTreeMap::new(),
        ..BundleSpec::default()
    };

    spec.pack("dist/my-server-1.2.3-darwin-arm64.mcpb".as_ref())
}
```

A bundle holds the binary of **one** platform: the manifest can switch the command per operating
system but not per CPU architecture, so each OS/architecture is its own bundle.
`BundleSpec::file_name` names it the way the releases do. The crate needs Rust 1.85 or later, since
it is edition 2024.

Everything but the name, the version, the description, the author, the platform and the executable
has a default, so `..BundleSpec::default()` keeps a spec to what it says.

Published as [`mcpb`](https://crates.io/crates/mcpb), with the API documentation on
[docs.rs](https://docs.rs/mcpb); before 1.0 the API is not stable.

## What goes into a bundle

```text
my-server-1.2.3-darwin-arm64.mcpb
├── manifest.json                  # written from the spec
├── icon.png                       # `icon`, at the root, where clients look for it
├── screenshot.png                 # `screenshots`, likewise
├── server/
│   ├── my-server                  # `executable`, mode 755
│   ├── helper                     # `extra_executables`, mode 755
│   └── THIRD-PARTY-NOTICES.md     # `resources`, mode 644
└── mcpb-resources/
    └── de-DE.json                 # `localization`, mode 644
```

The manifest is written in version 0.3 (`MANIFEST_VERSION`) with the name, version, description and
author (`name`, `email`, `url`) of the server; the metadata stores show (`long_description`,
`repository`, `homepage`, `documentation`, `support`, `license`, `keywords`, `privacy_policies`,
`_meta`); its icon(s) and screenshots; tools and prompts with their `_generated` flags; its
`compatibility.platforms`; what it asks the user for at install (`user_config`: text, numbers, tick
boxes, directories and files); and `server.mcp_config`: the command (`${__dirname}/server/…`), the
arguments, the environment (`env`), and the per-platform overrides.

The first icon without a theme is also written as the manifest's plain `icon`, which clients that
read only that field show. `platform_overrides` describes what the bundle's own platform runs
instead; a bundle names one platform, so an override for another one is refused.

`pack` writes the bundle under a temporary name and moves it into place, so a failed pack leaves no
partial bundle behind; `pack_to` writes the same bundle to any `Write + Seek`, such as a
`Cursor` over a `Vec` in memory. Both write the entries in a fixed order with a fixed timestamp, so
the same spec packs to the same bytes every time. It refuses two different files that would share a
name in the zip; the same file used twice, as the plain icon and as an icon variant, is written
once.

What went wrong comes back as an [`mcpb::Error`](Error): `Invalid` for the problems `validate`
reports, and then the file that could not be read or the bundle that could not be written.

## Checks

`BundleSpec::validate` reports everything clients would refuse, and `pack` refuses to write such a
spec. It looks at the spec alone:

- the name may only hold ASCII letters, digits, `-`, `_` and `.`, and cannot be `.` or `..`, since
  it names the executable and the bundle file
- the version is a semantic version
- description, author and the directory prompt say something, and so do every tool's description
  and every prompt's text
- the author's email and the URLs (`author.url`, `homepage`, `documentation`, `support`,
  `repository.url`, `privacy_policies`) look like what they claim
- a tool, a prompt and a prompt's arguments are each named once, as is every keyword, and a `_meta`
  namespace holds an object
- `args` and `env` only use the variables clients substitute: `${__dirname}`, `${HOME}`,
  `${DOCUMENTS}`, `${DOWNLOADS}`, `${DESKTOP}`, `${pathSeparator}`, `${/}`, and
  `${user_config.<key>}` of a key the manifest declares
- environment variables are named like environment variables
- icon sizes read `WIDTHxHEIGHT`, and locale codes read like `en-US`
- `platform_overrides` only names the bundle's own platform, the only one it can run on
- every `user_config` entry has a key of ASCII letters, digits, `_`, `-` and `.` that is used once, a
  title and a description, and a default that fits its type; `min` and `max` only bound a number,
  and `min` is not above `max`

`pack` additionally reads the files: an icon that is not a PNG is refused, since clients show
nothing else, and so is a second, different file whose name is already taken in the zip.

## Not yet

Signing (`mcpb sign`/`verify`). Bundles are unsigned, which `mcpb info` reports; sign them with the
`mcpb` CLI after packing if you need a signature.

[TODO.md](TODO.md) is the full list, both of what the spec has and what the crate still needs to be
useful on its own: more `user_config` kinds, the rest of the manifest's metadata fields, `_meta`,
nested files with `.mcpbignore`, reading a bundle back, and a `cargo install mcpb` binary.

## Releasing

Tag the commit whose `Cargo.toml` version is the one to release; the tag is `v<version>` and the
changelog section for that version has to be dated, not `unreleased`:

```sh
git tag -a v0.2.0 -m v0.2.0
git push origin v0.2.0
```

`.github/workflows/release.yml` then publishes the crate to crates.io through Trusted Publishing
and creates the GitHub release with that changelog section as its notes. A tag that does not name
the crate's version, or a changelog entry without a date, is refused before anything is published;
a re-run skips a version crates.io already has and still creates the release.

## Changelog

What changed in each release: [CHANGELOG.md](https://github.com/Synphonyte/mcpb/blob/main/CHANGELOG.md).

## License

MIT — see [LICENSE](https://github.com/Synphonyte/mcpb/blob/main/LICENSE).
