# mcpb

[![CI](https://github.com/Synphonyte/mcpb/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Synphonyte/mcpb/actions/workflows/ci.yml)

Pack a local MCP server as an [MCP Bundle](https://github.com/modelcontextprotocol/mcpb) (`.mcpb`): a
zip with a `manifest.json` and the server binary, which clients such as Claude Desktop install with
one click. No Node or `mcpb` CLI needed.

```sh
cargo add mcpb                # as a library
cargo binstall mcpb           # as a command, prebuilt
cargo install --locked mcpb   # as a command, compiled
```

Starting a new server? [`template/`](https://github.com/Synphonyte/mcpb/tree/main/template) is a
minimal [`rmcp`](https://crates.io/crates/rmcp) server whose release workflow attaches a bundle
for each platform to every GitHub release. Already have one? The [GitHub Action](#github-action)
packs it in your own workflow.

```rust,no_run
use std::collections::BTreeMap;

use mcpb::{Author, BundleSpec, Executable, Icon, Platform, Prompt, UserConfig};

fn main() -> Result<(), mcpb::Error> {
    let spec = BundleSpec {
        name: "my-server".into(),
        version: "1.2.3".parse()?,
        description: "What the server does.".into(),
        author: Author::new("Me"),
        executables: BTreeMap::from([(
            Platform::Darwin,
            Executable::new("target/release/my-server", "arm64"),
        )]),
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
        ..BundleSpec::default()
    };

    spec.pack("dist/my-server-1.2.3-darwin-arm64.mcpb".as_ref())
}
```

Only the name, version, description, author and executables are required; everything else has a
default. `Platform` uses the manifest's spelling (`darwin`, `linux`, `win32`), and `Platform::parse`
accepts nothing else.

A binary only runs where it was built, and the manifest can switch the command per operating system
but not per CPU architecture, so a bundle usually targets one OS and architecture. Give
`executables` several platforms and each one starts its own binary through `platform_overrides`;
`BundleSpec::file_name` names the bundle after them.

Requires Rust 1.85 (edition 2024). Published on [crates.io](https://crates.io/crates/mcpb), with
API docs on [docs.rs](https://docs.rs/mcpb). The API is not stable before 1.0.

## Command line

```sh
mcpb pack path/to/manifest.json --arch arm64 -o my-server.mcpb
mcpb pack manifest.json --platform linux --arch x64 --executable target/release/my-server -o dist/
mcpb validate my-server.mcpb
mcpb info my-server.mcpb
```

- `pack` resolves paths relative to the manifest and names the bundle
  `<name>-<version>-<platform>-<arch>.mcpb` unless `-o` names a file rather than a directory.
  `--platform` packs only one of the platforms the manifest lists, as a build for one OS needs,
  and `--executable` takes the binary from where cargo built it rather than the entry point's
  path. `--tree dir` packs a directory into `server/` as executables, honouring `.mcpbignore`;
  `--data dir` packs it as plain files.
- `validate` checks a manifest, or a bundle and that it holds the files its manifest names.
- `info` prints what a bundle holds, with `*` marking executables:

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

The same information is available to Rust code through [`Bundle`].

## GitHub Action

`uses: Synphonyte/mcpb@v0` packs the binary a job just built for the runner's OS and CPU, checks
the bundle and outputs its path, so a matrix of runners gives one bundle per platform:

```yaml
jobs:
  bundle:
    strategy:
      matrix:
        os: [ubuntu-latest, ubuntu-24.04-arm, macos-latest, windows-latest, windows-11-arm]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v6
      - run: cargo build --release --locked
      - uses: Synphonyte/mcpb@v0
        id: mcpb
        with:
          executable: target/release/my-server # `.exe` is added on Windows
          out: dist/
      - uses: actions/upload-artifact@v4
        with:
          name: ${{ steps.mcpb.outputs.name }}
          path: ${{ steps.mcpb.outputs.bundle }}
```

| Input | Default | |
| --- | --- | --- |
| `manifest` | `manifest.json` | the manifest, or its directory |
| `executable` | the entry point | the built binary, relative to the workspace |
| `platform` | the runner's OS | `darwin`, `linux` or `win32` |
| `arch` | the runner's CPU | `x64`, `arm64` …; set it when cross-compiling |
| `out` | the bundle's name | a file, or a directory ending in `/` |
| `args` | | more `mcpb pack` arguments, such as `--tree assets` |
| `mcpb-version` | the action's | another release, or `source` to build the action's checkout |

The outputs are `bundle`, the bundle's path, and `name`, its file name. The action downloads the
`mcpb` binary of its own release, and builds it with cargo where there is none. The template's
[release workflow](https://github.com/Synphonyte/mcpb/blob/main/template/.github/workflows/release.yml)
shows the whole thing: six targets, then one GitHub release with every bundle.

## Bundle layout

```text
my-server-1.2.3-darwin-arm64-win32-x64.mcpb
├── manifest.json                  # written from the spec
├── icon.png                       # `icon`, at the root unless a `BundleFile` places it
├── screenshot.png                 # `screenshots`, likewise
├── server/
│   ├── my-server                  # the first platform's binary, mode 755
│   ├── my-server-win32.exe        # each further platform's binary, mode 755
│   ├── helper                     # `extra_executables`, mode 755
│   ├── THIRD-PARTY-NOTICES.md     # `resources`, mode 644
│   └── lib/…                      # `trees`, as laid out on disk
└── mcpb-resources/
    └── de-DE.json                 # `localization`, mode 644
```

The manifest is written in version 0.3 (`MANIFEST_VERSION`). It holds the metadata (author,
`long_description`, `repository`, `homepage`, `documentation`, `support`, `license`, `keywords`,
`privacy_policies`, `_meta`), icons and screenshots, tools and prompts, `compatibility.platforms`,
`user_config`, and `server.mcp_config` with the command, arguments, `env` and per-platform
overrides. The first icon without a theme is also written as the plain `icon`, for clients that
only read that field.

A [`Tree`] packs a directory as it is laid out, for data files, packages or a script's assets. It
skips what the format always excludes (`.git/`, `*.log`, `node_modules/.cache/`, lockfiles,
`.mcpbignore` …) and what the `.mcpbignore` at its root lists, with `.gitignore` semantics.
Symlinks are followed. A link back into a directory above it is skipped, and a dangling link is an
error unless a pattern ignores it. [`Tree::new`] packs executables into `server/`, `Tree::data`
packs plain files, and `Tree::in_dir` picks another directory:

```rust,no_run
# fn main() -> Result<(), mcpb::Error> {
# let mut spec = mcpb::BundleSpec { name: "my-server".into(), version: "1.2.3".parse()?,
#     description: "What the server does.".into(), author: mcpb::Author::new("Me"),
#     executables: Default::default(), ..mcpb::BundleSpec::default() };
spec.trees = vec![
    mcpb::Tree::new("server-files"),
    mcpb::Tree::data("data").in_dir("server/data"),
];
# Ok(())
# }
```

A [`BundleFile`] puts a single file at any path, such as `server/lib/libcalculator.dylib`. An icon
or screenshot whose path matches a `BundleFile` goes there, and the manifest points there;
otherwise it goes to the root.

`pack` writes to a temporary file and renames it, so a failed pack leaves nothing behind. `pack_to`
writes to any `Write + Seek`. Output is reproducible: entries are sorted and use a fixed timestamp.
Two different files with the same name are refused; the same file used twice is written once. A
bundle is never packed into itself.

Errors are an [`mcpb::Error`](Error), one variant per kind of failure: `Invalid` (what `validate`
reports), `Version`, `Read`, `Write`, `Name` (no UTF-8 file name), `Icon` (not a PNG), `Clash` (two
files, one name), and `Bundle`, `Unsafe` and `Manifest` for bundles and manifests that cannot be
read.

## Reading a bundle

[`Bundle::open`] reads any existing `.mcpb`, whatever packed it:

```rust,no_run
let bundle = mcpb::Bundle::open("dist/my-server-1.2.3-darwin-arm64.mcpb")?;
let manifest = bundle.manifest();
println!("{} {}, signed: {}", manifest["name"], manifest["version"], bundle.is_signed());
for entry in bundle.entries() {
    println!("{:>9}  {}{}", entry.size, entry.name, if entry.executable { " *" } else { "" });
}
bundle.extract_to("dist/unpacked")?;
# Ok::<(), mcpb::Error>(())
```

[`Bundle::manifest`] returns the manifest, [`Bundle::entries`] the files with their sizes and
modes, [`Bundle::is_signed`] whether a signature follows the zip, and [`Bundle::extract_to`]
unpacks it, refusing paths that would escape the target directory.

## Editing a manifest

[`BundleSpec::from_manifest`] turns a manifest into a spec and `manifest()` writes it back, so a
bundle can be repacked with changes:

```rust,no_run
let bundle = mcpb::Bundle::open("dist/my-server-1.2.3-darwin-arm64.mcpb")?;
bundle.extract_to("dist/unpacked")?;

let mut spec = bundle.spec()?.in_dir("dist/unpacked");
spec.version = "1.2.4".parse()?;
spec.pack(format!("dist/{}", spec.file_name()).as_ref())?;
# Ok::<(), mcpb::Error>(())
```

`from_manifest` leaves out what a manifest doesn't record; [`Bundle::spec`] reads it from the
bundle instead: binary architectures, the helpers and resources in `server/`, and the locales. Any
other file comes back as a [`BundleFile`] at its original path, so repacking keeps every file in
place. `from_manifest` only reads binary servers whose `mcp_config.command` runs an `entry_point`
under `server/`, and refuses anything else. `user_config` keeps its order. `BundleSpec` also
implements `Serialize` and `Deserialize` in the manifest's shape.

## Checks

`BundleSpec::validate` reports everything clients would refuse, and `pack` won't write a spec that
fails it:

- the name uses only ASCII letters, digits, `-`, `_` and `.`, and is not `.` or `..`
- there is at least one executable, architectures and names are valid file names, and no two
  binaries share a name
- every tree and `BundleFile` stays inside the bundle
- description, author, `display_name`, `long_description`, `license`, repository type, tool
  descriptions, prompt texts and `user_config` fields are not empty
- emails and URLs (`author.url`, `homepage`, `documentation`, `support`, `repository.url`,
  `privacy_policies`) look valid
- tool, prompt, prompt argument and keyword names are unique, and each `_meta` namespace is an
  object
- `args` and `env` only use variables clients substitute: `${__dirname}`, `${HOME}`,
  `${DOCUMENTS}`, `${DOWNLOADS}`, `${DESKTOP}`, `${pathSeparator}`, `${/}`, and
  `${user_config.<key>}` for declared keys
- environment variable names, `WIDTHxHEIGHT` icon sizes and `en-US`-style locales are well formed
- `platform_overrides` only names platforms that have an executable
- `user_config` keys (ASCII letters, digits, `_`, `-`, `.`) are unique, defaults match their type,
  and `min`/`max` only bound numbers, with `min` not above `max`

The version is a [`Version`], so it is always valid semver. `pack` also refuses icons that aren't
PNGs and file name clashes. `mcpb validate` on a bundle also checks that the entry point, each
platform's binary, the icons and the screenshots are present.

## Tests

`cargo test` validates generated manifests against the format's JSON schema (`tests/schemas/`,
from the MCP Bundle repository), and round-trips the upstream
[`examples/calculator-rust`](https://github.com/modelcontextprotocol/mcpb/tree/main/examples/calculator-rust)
manifest and `.mcpbignore` (vendored in `tests/fixtures/`).

CI runs on Linux, macOS and Windows, x64 and arm64, and on Rust 1.85 with the committed
`Cargo.lock`. On each runner it also packs real bundles with the library and the CLI, and has the
official `mcpb` CLI unpack and validate them.

## Not supported

- **Signing.** Bundles are unsigned, as `mcpb info` and [`Bundle::is_signed`] report; sign them
  with the official `mcpb sign` after packing. Signing would need a crypto stack, and clients only
  trust CA-issued certificates, so a self-signed signature adds little. The signature is a PKCS#7
  trailer after the zip, so support can be added later without changing what is packed.
- **Non-binary servers** (node, python, `uv`). The point is to ship no runtime; `from_manifest`
  refuses them.
- **Several CPU architectures in one bundle.** The manifest can't switch per architecture, so x64
  and arm64 need separate bundles, unless the binary is a universal macOS binary.
- **Manifest version 0.4.** It only adds the `uv` server type.
- **A custom localization path.** Translations always go to `mcpb-resources/${locale}.json`.

## Releasing

Set the version in `Cargo.toml`, rename the changelog's `Unreleased` heading to
`<version> - YYYY-MM-DD`, commit, then tag and push:

```sh
git tag -a v0.2.1 -m v0.2.1
git push origin main v0.2.1
```

`.github/workflows/release.yml` then publishes to crates.io through Trusted Publishing and creates
a GitHub release with that changelog section as its notes. It refuses a tag that doesn't match the
crate's version or a changelog entry without a date, and a re-run skips publishing a version
crates.io already has. It then attaches the `mcpb` binary for macOS, Linux (static, musl) and
Windows, each on x64 and arm64, as `mcpb-<target>.tgz` (`.zip` on Windows), which is what
`cargo binstall` looks for, and moves the `v0` tag that `uses: Synphonyte/mcpb@v0` names to the
release.

## Changelog

See [CHANGELOG.md](https://github.com/Synphonyte/mcpb/blob/main/CHANGELOG.md).

## License

MIT — see [LICENSE](https://github.com/Synphonyte/mcpb/blob/main/LICENSE).
