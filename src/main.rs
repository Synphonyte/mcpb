//! `mcpb` on the command line: pack, validate and inspect bundles without a
//! Rust program or the Node CLI.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use mcpb::{Bundle, BundleSpec, Error, Platform, Tree};

const USAGE: &str = "\
mcpb - pack, validate and inspect MCP Bundles (.mcpb)

usage:
  mcpb pack <manifest.json | directory> [-o <bundle.mcpb>] [--arch <arch>]
                                        [--platform <platform>] [--executable <file>]
                                        [--tree <dir>]... [--data <dir>]...
  mcpb validate <manifest.json | directory | bundle.mcpb>
  mcpb info <bundle.mcpb>

pack:
  Writes the bundle a manifest describes. Paths in the manifest are read
  relative to it, and the bundle is named after the spec
  (<name>-<version>-<platform>-<arch>.mcpb) unless -o says where to put it.

  -o, --out <path>   where to write the bundle, or the directory to write it to
      --arch <arch>  the CPU architecture of the binaries, such as arm64
      --platform <platform>
                     pack only this platform (darwin, linux or win32) of those
                     the manifest names, as a build for one OS does
      --executable <file>
                     the binary to pack as the entry point, such as
                     target/release/my-server, instead of the file the manifest
                     names; needs --platform when the manifest names several
      --tree <dir>   the files of <dir>, packed into server/ as they are laid out
                     and written as programs - `--tree server` for a directory of
                     that name; relative to the manifest, as its paths are
      --data <dir>   the same, written as files to read

validate:
  Checks a manifest against what clients require, or the manifest of a bundle.

info:
  What a bundle holds: its name and version, the platforms and the entry point,
  its size, whether it is signed, and every file with its size.

  -h, --help         this text
  -V, --version      the crate's version
";

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(Some(text)) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(Failure::Usage(problem)) => {
            eprintln!("mcpb: {problem}\n\n{USAGE}");
            ExitCode::from(2)
        }
        Err(Failure::Failed(error)) => {
            eprintln!("mcpb: {error}");
            ExitCode::FAILURE
        }
        Err(Failure::Invalid { what, problems }) => {
            eprintln!("mcpb: {} is not valid:", what.display());
            for problem in problems {
                eprintln!("  - {problem}");
            }
            ExitCode::FAILURE
        }
    }
}

/// Why the command failed: bad arguments, a failed operation, or an invalid
/// manifest or bundle.
enum Failure {
    Usage(String),
    Failed(Error),
    Invalid {
        what: PathBuf,
        problems: Vec<String>,
    },
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self::Failed(error)
    }
}

/// What to do, and with what.
enum Command {
    Pack {
        manifest: PathBuf,
        out: Option<PathBuf>,
        arch: Option<String>,
        platform: Option<Platform>,
        executable: Option<PathBuf>,
        trees: Vec<Tree>,
    },
    Validate(PathBuf),
    Info(PathBuf),
}

fn run(args: Vec<String>) -> Result<Option<String>, Failure> {
    let Some((command, rest)) = args.split_first() else {
        return Err(Failure::Usage("no command".to_string()));
    };
    if rest.iter().any(|arg| arg == "-h" || arg == "--help") {
        return Ok(Some(USAGE.to_string()));
    }

    match command.as_str() {
        "-h" | "--help" | "help" => Ok(Some(USAGE.to_string())),
        "-V" | "--version" => Ok(Some(format!("mcpb {}", env!("CARGO_PKG_VERSION")))),
        name => {
            let command = parse(name, rest)?;
            match command {
                Command::Pack {
                    manifest,
                    out,
                    arch,
                    platform,
                    executable,
                    trees,
                } => pack(&manifest, out, arch, platform, executable, trees).map(Some),
                Command::Validate(path) => validate(&path),
                Command::Info(path) => info(&path).map(Some),
            }
        }
    }
}

fn parse(command: &str, args: &[String]) -> Result<Command, Failure> {
    let mut positional = Vec::new();
    let mut out = None;
    let mut arch = None;
    let mut platform = None;
    let mut executable = None;
    let mut trees = Vec::new();

    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| {
            args.next()
                .cloned()
                .ok_or_else(|| Failure::Usage(format!("{flag} needs a value")))
        };

        match arg.as_str() {
            "-o" | "--out" => out = Some(PathBuf::from(value(arg)?)),
            "--arch" => arch = Some(value(arg)?),
            "--platform" => {
                let name = value(arg)?;
                platform = Some(Platform::parse(&name).ok_or_else(|| {
                    Failure::Usage(format!(
                        "--platform {name} is none of darwin, linux and win32"
                    ))
                })?);
            }
            "--executable" => executable = Some(PathBuf::from(value(arg)?)),
            "--tree" => trees.push(Tree::new(value(arg)?)),
            "--data" => trees.push(Tree::data(value(arg)?)),
            flag if flag.starts_with('-') => {
                return Err(Failure::Usage(format!("{flag} is no option of mcpb")));
            }
            path => positional.push(PathBuf::from(path)),
        }
    }

    let command = match command {
        "pack" => {
            let mut paths = positional.into_iter();
            let manifest = paths
                .next()
                .ok_or_else(|| Failure::Usage("pack needs a manifest".to_string()))?;
            if let Some(extra) = paths.next() {
                return Err(Failure::Usage(format!(
                    "pack takes one manifest, not {}",
                    extra.display()
                )));
            }
            Command::Pack {
                manifest,
                out,
                arch,
                platform,
                executable,
                trees,
            }
        }
        "validate" | "info" => {
            let mut paths = positional.into_iter();
            let path = paths
                .next()
                .ok_or_else(|| Failure::Usage(format!("{command} needs a bundle or a manifest")))?;
            if let Some(extra) = paths.next() {
                return Err(Failure::Usage(format!(
                    "{command} takes one file, not {}",
                    extra.display()
                )));
            }
            if command == "validate" {
                Command::Validate(path)
            } else {
                Command::Info(path)
            }
        }
        other => {
            return Err(Failure::Usage(format!("{other} is no command of mcpb")));
        }
    };

    Ok(command)
}

fn pack(
    manifest: &Path,
    out: Option<PathBuf>,
    arch: Option<String>,
    platform: Option<Platform>,
    executable: Option<PathBuf>,
    trees: Vec<Tree>,
) -> Result<String, Failure> {
    let dir = base_dir(manifest);
    let path = if manifest.is_dir() {
        manifest.join("manifest.json")
    } else {
        manifest.to_path_buf()
    };

    let mut spec = read_manifest(&path)?.in_dir(&dir);
    if let Some(platform) = platform {
        if !spec.executables.contains_key(&platform) {
            return Err(Failure::Usage(format!(
                "--platform {}: {} names no such platform in compatibility.platforms",
                platform.as_str(),
                path.display()
            )));
        }
        // A binary only runs on the OS it was built for, so the bundle claims
        // that platform alone, and drops the others' overrides with them.
        spec.executables.retain(|other, _| *other == platform);
        spec.platform_overrides
            .retain(|other, _| *other == platform);
    }
    if let Some(executable) = executable {
        let mut executables = spec.executables.values_mut();
        match (executables.next(), executables.next()) {
            (Some(only), None) => only.path = dir.join(executable),
            _ => {
                return Err(Failure::Usage(
                    "--executable needs --platform, since the manifest names several platforms"
                        .to_string(),
                ));
            }
        }
    }
    if let Some(arch) = arch {
        for executable in spec.executables.values_mut() {
            executable.arch = Some(arch.clone());
        }
    }
    // Trees are relative to the manifest, like its other paths.
    spec.trees.extend(trees.into_iter().map(|tree| Tree {
        path: dir.join(&tree.path),
        ..tree
    }));

    let out = match out {
        Some(dir) if dir.is_dir() => dir.join(spec.file_name()),
        Some(out) => out,
        None => PathBuf::from(spec.file_name()),
    };
    spec.pack(&out)?;

    let bundle = Bundle::open(&out)?;
    Ok(format!(
        "packed {} ({} files, {}, {})",
        out.display(),
        bundle.entries().len(),
        size(bundle.size()),
        match bundle.is_signed() {
            true => "signed",
            false => "unsigned",
        }
    ))
}

/// The manifest's spec, with its paths joined onto the manifest's directory.
fn read_manifest(path: &Path) -> Result<BundleSpec, Failure> {
    let text = fs::read_to_string(path).map_err(|error| {
        Failure::Failed(Error::Read {
            path: path.to_path_buf(),
            source: error,
        })
    })?;
    let manifest = serde_json::from_str(&text).map_err(|error| {
        Failure::Failed(Error::Manifest(format!(
            "{} is not JSON: {error}",
            path.display()
        )))
    })?;

    Ok(BundleSpec::from_manifest(&manifest)?)
}

fn base_dir(path: &Path) -> PathBuf {
    if path.is_dir() {
        return path.to_path_buf();
    }
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn validate(path: &Path) -> Result<Option<String>, Failure> {
    if is_bundle(path) {
        let bundle = Bundle::open(path)?;
        let spec = BundleSpec::from_manifest(bundle.manifest())?;
        return valid(&spec, path, Some(&bundle));
    }

    let manifest = match path.is_dir() {
        true => path.join("manifest.json"),
        false => path.to_path_buf(),
    };
    valid(&read_manifest(&manifest)?, &manifest, None)
}

/// Whether the file is a zip (starts with `PK`) rather than a manifest.
fn is_bundle(path: &Path) -> bool {
    use std::io::Read;

    let mut start = [0; 2];
    match fs::File::open(path).and_then(|mut file| file.read_exact(&mut start)) {
        Ok(()) => start == *b"PK",
        Err(_) => false,
    }
}

/// What `validate` reports, for a manifest or a bundle.
fn valid(
    spec: &BundleSpec,
    what: &Path,
    bundle: Option<&Bundle>,
) -> Result<Option<String>, Failure> {
    let mut problems = spec.validate().err().unwrap_or_default();
    if let Some(bundle) = bundle {
        problems.extend(missing(bundle));
    }
    if !problems.is_empty() {
        return Err(Failure::Invalid {
            what: what.to_path_buf(),
            problems,
        });
    }

    let mut text = format!(
        "{} is valid: {} {} on {}",
        what.display(),
        spec.name,
        spec.version,
        spec.executables
            .keys()
            .map(|platform| platform.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    if let Some(bundle) = bundle.filter(|bundle| !bundle.is_current()) {
        text.push_str(&format!(
            ", manifest version {}",
            bundle.manifest_version().unwrap_or("?")
        ));
    }

    Ok(Some(text))
}

/// The files the manifest names that the bundle lacks: the entry point, each
/// platform's binary, the icons and the screenshots.
fn missing(bundle: &Bundle) -> Vec<String> {
    let manifest = bundle.manifest();
    let mut named: Vec<(&str, &str)> = Vec::new();

    if let Some(entry_point) = bundle.entry_point() {
        named.push(("server.entry_point", entry_point));
    }
    if let Some(overrides) = manifest["server"]["mcp_config"]["platform_overrides"].as_object() {
        for config in overrides.values() {
            if let Some(path) = config["command"]
                .as_str()
                .and_then(|command| command.strip_prefix("${__dirname}/"))
            {
                named.push(("a platform override's command", path));
            }
        }
    }
    if let Some(icon) = manifest["icon"].as_str() {
        named.push(("icon", icon));
    }
    for icon in manifest["icons"].as_array().into_iter().flatten() {
        if let Some(src) = icon["src"].as_str() {
            named.push(("icons", src));
        }
    }
    for screenshot in manifest["screenshots"].as_array().into_iter().flatten() {
        if let Some(path) = screenshot.as_str() {
            named.push(("screenshots", path));
        }
    }

    named
        .into_iter()
        .filter(|(_, path)| !bundle.entries().iter().any(|entry| entry.name == *path))
        .map(|(what, path)| format!("{what} names {path}, which the bundle does not hold"))
        .collect()
}

fn info(path: &Path) -> Result<String, Failure> {
    let bundle = Bundle::open(path)?;
    let manifest = bundle.manifest();

    let mut text = format!(
        "{} {}\n  bundle: {} ({})\n  manifest version: {}\n  platforms: {}\n  entry point: {}",
        manifest["name"].as_str().unwrap_or("?"),
        manifest["version"].as_str().unwrap_or("?"),
        path.display(),
        size(bundle.size()),
        bundle.manifest_version().unwrap_or("?"),
        platforms(&bundle),
        bundle.entry_point().unwrap_or("?"),
    );
    text.push_str(&format!(
        "\n  signed: {}",
        match bundle.is_signed() {
            true => "yes (not verified)",
            false => "no",
        }
    ));

    text.push_str("\n  files:");
    for entry in bundle.entries() {
        text.push_str(&format!(
            "\n    {:>9}  {}{}",
            size(entry.size),
            entry.name,
            if entry.executable { " *" } else { "" }
        ));
    }

    Ok(text)
}

fn platforms(bundle: &Bundle) -> String {
    bundle.manifest()["compatibility"]["platforms"]
        .as_array()
        .map(|platforms| {
            platforms
                .iter()
                .filter_map(|platform| platform.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// A human-readable size.
fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }

    match unit {
        0 => format!("{bytes} B"),
        _ => format!("{size:.1} {}", UNITS[unit]),
    }
}
