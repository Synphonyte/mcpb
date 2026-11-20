# mcpb

Pack a local MCP server as an [MCP Bundle](https://github.com/modelcontextprotocol/mcpb) (`.mcpb`): a
zip with a `manifest.json` and the server binary, which clients such as Claude Desktop install with
one click. No Node and no `mcpb` CLI.

```sh
cargo add mcpb
```

```rust
use mcpb::{BundleSpec, DirectoryPrompt, Platform};

let spec = BundleSpec {
    name: "my-server".into(),
    display_name: None,
    version: "1.2.3".into(),
    description: "What the server does.".into(),
    author: "Me".into(),
    platform: Platform::Darwin,
    executable: "target/release/my-server".into(),
    extra_executables: Vec::new(),
    resources: Vec::new(),
    tools: vec![("whoami".into(), "Who the caller is.".into())],
    directory: Some(DirectoryPrompt {
        title: "Project directory".into(),
        description: "The directory the server works in.".into(),
    }),
    args: Vec::new(),
};

spec.pack("dist/my-server-1.2.3-darwin-arm64.mcpb".as_ref())?;
```

A bundle holds the binary of **one** platform: the manifest can switch the command per operating
system but not per CPU architecture, so each OS/architecture is its own bundle.
`BundleSpec::file_name` names it the way the releases do.

## Status

Published as [`mcpb`](https://crates.io/crates/mcpb) 0.1.0, which reserves the name; the API is
not stable yet.

Writes the `manifest.json` (version 0.3) and the archive: executables with mode 755, resources with
644, under a temporary name so a failed pack leaves no partial bundle, and two files that would share
a name in the zip are refused.

Not yet: manifest validation, icons, `server.mcp_config.env`,
`compatibility.platform_overrides`, prompts, localization, and signing (`mcpb sign`/`verify`).
[TODO.md](TODO.md) is the full list, both of what the spec has and what the crate still needs to be
useful on its own.

## License

MIT — see [LICENSE](LICENSE).
