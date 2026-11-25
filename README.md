# mcpb

[![CI](https://github.com/Synphonyte/mcpb/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Synphonyte/mcpb/actions/workflows/ci.yml)

Pack a local MCP server as an [MCP Bundle](https://github.com/modelcontextprotocol/mcpb) (`.mcpb`): a
zip with a `manifest.json` and the server binary, which clients such as Claude Desktop install with
one click. No Node and no `mcpb` CLI.

```sh
cargo add mcpb          # as a library
cargo install --locked mcpb   # or as a command
```

```rust,no_run
use std::collections::BTreeMap;

use mcpb::{Author, BundleSpec, Executable, Icon, Platform, Prompt, UserConfig};

fn main() -> Result<(), mcpb::Error> {
    let spec = BundleSpec {
        name: "my-server".into(),
        display_name: None,
        version: "1.2.3".parse()?,
        description: "What the server does.".into(),
        author: Author::new("Me"),
        executables: BTreeMap::from([(
            Platform::Darwin,
            Executable::new("target/release/my-server", "arm64"),
        )]),
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

A bundle holds one server binary per platform it runs on. Usually that is one platform, because a
binary only runs where it was built: the manifest can switch the command per operating system but
not per CPU architecture, so each OS/architecture is its own bundle. Give `executables` several
platforms and the bundle starts each platform's own binary through `platform_overrides`;
`BundleSpec::file_name` names the bundle after them, the way the releases do. The crate needs Rust
1.85 or later, since it is edition 2024; `Cargo.lock` is committed and CI runs `cargo test --locked`
on 1.85, so that promise is checked against the versions that were tested.

Everything but the name, the version, the description, the author, the platform and the executable
has a default, so `..BundleSpec::default()` keeps a spec to what it says. The version is a
[`Version`], so `"1.2.3".parse()?` either gives one or fails where the spec is built; `Platform` is
named as the manifest spells it (`darwin`, `linux`, `win32`), and `Platform::parse` takes nothing
else, so a release script that says `macos` hears about it.

Published as [`mcpb`](https://crates.io/crates/mcpb), with the API documentation on
[docs.rs](https://docs.rs/mcpb); before 1.0 the API is not stable.

## On the command line

`cargo install mcpb` gives the same thing without a Rust program in the way: `pack` a manifest,
`validate` a manifest or a bundle, `info` a bundle.

```sh
mcpb pack path/to/manifest.json --arch arm64 -o my-server.mcpb
mcpb validate my-server.mcpb
mcpb info my-server.mcpb
```

`pack` reads the manifest's paths relative to the manifest, names the bundle
`<name>-<version>-<platform>-<arch>.mcpb` unless `-o` says where to put it, and `--tree dir` packs the
files of a directory into `server/` as they are laid out (as `.mcpbignore` leaves them) for a server
with files beside its binary - `--data dir` writes them as files to read rather than as programs.
`info` prints what a bundle holds:

```text
calculator-rust 1.0.0
  bundle: out.mcpb (893 B)
  manifest version: 0.3
  platforms: darwin, linux
  entry point: server/mcp-calculator
  signed: no
  files:
      1.1 KiB  manifest.json
          9 B  server/libcalculator.dylib *
         19 B  server/mcp-calculator *
```

`*` marks a file written as a program. Everything a person reads there is what [`Bundle`] hands a
Rust program as well, so a release check can be a few lines instead of a session at a terminal.

## What goes into a bundle

```text
my-server-1.2.3-darwin-arm64-win32-x64.mcpb
├── manifest.json                  # written from the spec
├── icon.png                       # `icon`, at the root, where clients look for it
├── screenshot.png                 # `screenshots`, likewise
├── server/
│   ├── my-server                  # the first platform's binary, mode 755
│   ├── my-server-win32.exe        # every further platform's, mode 755
│   ├── helper                     # `extra_executables`, mode 755
│   ├── THIRD-PARTY-NOTICES.md     # `resources`, mode 644
│   └── lib/…                      # `trees`, laid out as they are on disk
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
read only that field show. Each platform of the bundle starts its own binary: the manifest's
`entry_point` is the first platform's file and every other platform overrides the command, unless
the spec gives that platform a wrapper of its own to run.

What a server needs beside it that is more than a file - data files, packages, the assets of a script
it runs - is a [`Tree`]: a directory packed as it is laid out, into `server/` or anywhere else in
the bundle. A tree is packed without what the format leaves out anyway (`.git/`, `*.log`,
`node_modules/.cache/`, lockfiles, `.mcpbignore` …) and without what the `.mcpbignore` at its root
names, read as a `.gitignore` is: a pattern that starts with `/` or holds one in its middle
matches from the tree's root only, any other at any depth. A symlink is packed as what it points
at, a directory reached by two names is packed under both, and a link back into a directory the
walk is inside is not followed; a link to nothing is refused unless a pattern leaves it out.
[`Tree::new`] writes its files as programs, `Tree::data` as files to read, and `Tree::in_dir` puts
them somewhere other than `server/`:

```rust,no_run
# fn main() -> Result<(), mcpb::Error> {
# let mut spec = mcpb::BundleSpec { name: "my-server".into(), version: "1.2.3".parse()?,
#     description: "What the server does.".into(), author: mcpb::Author::new("Me"),
#     executables: Default::default(), ..mcpb::BundleSpec::default() };
spec.trees = vec![
    // What the server runs, next to the binary.
    mcpb::Tree::new("server-files"),
    // The data it reads, into `server/data/`.
    mcpb::Tree::data("data").in_dir("server/data"),
];
# Ok(())
# }
```

A single file goes wherever a [`BundleFile`] names it, such as `server/lib/libcalculator.dylib`. A
screenshot whose path is also a `BundleFile` is written there, and the manifest names it there, such
as `assets/shot.png`; any other screenshot goes to the bundle's root.

`pack` writes the bundle under a temporary name and moves it into place, so a failed pack leaves no
partial bundle behind; `pack_to` writes the same bundle to any `Write + Seek`, such as a
`Cursor` over a `Vec` in memory. Both write the entries in a fixed order with a fixed timestamp, so
the same spec packs to the same bytes every time - the files are sorted by their name in the bundle,
whatever order the directory they came from listed them in. It refuses two different files that
would share a name in the zip; the same file used twice, as the plain icon and as an icon variant, is
written once. Neither packs the bundle it is writing, so a bundle written into a directory it packs
does not pack the previous one.

What went wrong comes back as an [`mcpb::Error`](Error): `Invalid` for the problems `validate`
reports, and then the file that could not be read or the bundle that could not be written.

## Reading a bundle back

[`Bundle::open`] reads a `.mcpb` that already exists - whatever packed it - for checking what a
release actually contains:

```rust,no_run
let bundle = mcpb::Bundle::open("dist/my-server-1.2.3-darwin-arm64.mcpb")?;
println!("{} {}, signed: {}", bundle.manifest()["name"], bundle.manifest()["version"], bundle.is_signed());
for entry in bundle.entries() {
    println!("{:>9}  {}{}", entry.size, entry.name, if entry.executable { " *" } else { "" });
}
bundle.extract_to("dist/unpacked")?;
# Ok::<(), mcpb::Error>(())
```

[`Bundle::manifest`] is the manifest as it was written, [`Bundle::entries`] the files with their
sizes and whether they are written as programs, [`Bundle::is_signed`] whether a signature follows
the zip (this crate signs neither way; sign with the `mcpb` CLI), and [`Bundle::extract_to`]
unpacks them, refusing a name that would leave the directory.

## Editing a manifest

[`BundleSpec::from_manifest`] turns any manifest into a spec, and `manifest()` writes it again, so a
bundle can be read, changed and packed anew - the same bundle with a new version, say:

```rust,no_run
let bundle = mcpb::Bundle::open("dist/my-server-1.2.3-darwin-arm64.mcpb")?;
bundle.extract_to("dist/unpacked")?;

// The manifest names the files; `in_dir` finds them where they were unpacked.
let mut spec = bundle.spec()?.in_dir("dist/unpacked");
spec.version = "1.2.4".parse()?;
spec.pack(format!("dist/{}", spec.file_name()).as_ref())?;
# Ok::<(), mcpb::Error>(())
```

What the manifest does not carry - the architecture of a binary, the helpers and resources next to
the server, the locales - is what [`Bundle::spec`] reads from the files themselves;
`from_manifest` alone leaves those empty, since a manifest does not name them. A file deeper than
`server/`, or anywhere else the manifest does not point, comes back as a [`BundleFile`] under the
name it had, so packing the spec again puts every file where it was. `from_manifest` reads a binary
server only, whose `mcp_config.command` starts its `entry_point` somewhere under `server/`
(`server/bin/my-server` stays where it is); anything else is refused rather than rewritten.
`user_config` keeps the order the manifest gives it, which is the order clients ask in. `BundleSpec` is also
`Serialize` and `Deserialize` in the manifest's shape, so a caller can keep one in its own
configuration file.

## Checks

`BundleSpec::validate` reports everything clients would refuse, and `pack` refuses to write such a
spec. It looks at the spec alone:

- the name may only hold ASCII letters, digits, `-`, `_` and `.`, and cannot be `.` or `..`, since
  it names the executable and the bundle file
- the version is a semantic version, which [`Version`] enforces before a spec can hold one
- description, author and every `user_config` entry say something, and so do every tool's
  description and every prompt's text
- the author's email and the URLs (`author.url`, `homepage`, `documentation`, `support`,
  `repository.url`, `privacy_policies`) look like what they claim
- a tool, a prompt and a prompt's arguments are each named once, as is every keyword, and a `_meta`
  namespace holds an object
- `args` and `env` only use the variables clients substitute: `${__dirname}`, `${HOME}`,
  `${DOCUMENTS}`, `${DOWNLOADS}`, `${DESKTOP}`, `${pathSeparator}`, `${/}`, and
  `${user_config.<key>}` of a key the manifest declares
- environment variables are named like environment variables
- icon sizes read `WIDTHxHEIGHT`, and locale codes read like `en-US`
- `platform_overrides` names a platform the bundle has an executable for: the one starting that
  platform's own binary, or a wrapper around it
- every `user_config` entry has a key of ASCII letters, digits, `_`, `-` and `.` that is used once, a
  title and a description, and a default that fits its type; `min` and `max` only bound a number,
  and `min` is not above `max`

`pack` additionally reads the files: an icon that is not a PNG is refused, since clients show
nothing else, and so is a second, different file whose name is already taken in the zip. `mcpb
validate` on a bundle also checks that the bundle holds what its manifest names: the entry point,
each platform's own binary, the icons and the screenshots.

`cargo test` also validates what [`manifest`](BundleSpec::manifest) writes against the manifest
format's own JSON schema (`tests/schemas/`, taken from the MCP Bundle repository), so what clients
accept is checked rather than assumed from our own expectations.

## Not yet

Signing (`mcpb sign`/`verify`). Bundles are unsigned, which `mcpb info` and [`Bundle::is_signed`]
report; sign them with the `mcpb` CLI after packing if you need a signature. A server type other
than `binary` - node, python, `uv` - is out of scope: the crate's point is that there is no runtime
to ship.

[TODO.md](TODO.md) is the full list of what the crate still needs, and of what the spec has that a
binary bundle has no use for.

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
