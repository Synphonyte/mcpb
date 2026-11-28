//! `mcpb` on the command line: pack, sign, verify, validate and inspect bundles
//! without a Rust program or the Node CLI.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use mcpb::{
    Bundle, BundleSpec, CertificateValidity, CryptoValidity, Error, Platform, SignOptions,
    Signature, SignatureState, SigningKey, Tree, Trust,
};

const USAGE: &str = "\
mcpb - pack, sign, validate and inspect MCP Bundles (.mcpb)

usage:
  mcpb pack <manifest.json | directory> [-o <bundle.mcpb>] [--arch <arch>]
                                        [--platform <platform>] [--executable <file>]
                                        [--tree <dir>]... [--data <dir>]...
  mcpb sign <bundle.mcpb> --cert <cert.pem> --key <key.pem> [--chain <ca.pem>]...
                          [--force] [--passphrase-file <file>]
  mcpb verify <bundle.mcpb> [--no-trust | --ca-file <ca.pem>] [--allow-self-signed]
                            [--allow-expired] [--at <time>]
  mcpb unsign <bundle.mcpb>
  mcpb prepare-for-signing <bundle.mcpb> [--slot <bytes>]
  mcpb apply-signature <bundle.mcpb> --signature <sig.der>
  mcpb validate <manifest.json | directory | bundle.mcpb>
  mcpb info <bundle.mcpb>

pack:
  Writes the bundle a manifest describes. Its own paths are read relative to
  the manifest; the paths given here, -o and --executable, are read from where
  you run mcpb. The bundle is named after the spec
  (<name>-<version>-<platform>-<arch>.mcpb) unless -o says where to put it.

  -o, --out <path>   where to write the bundle, or the directory to write it
                     to; a path ending with / is a directory, made if missing
      --arch <arch>  the CPU architecture of the binaries, such as arm64
      --platform <platform>
                     pack only this platform (darwin, linux or win32) of those
                     the manifest names, as a build for one OS does
      --executable <file>
                     the binary to pack as the entry point, such as
                     target/release/my-server, instead of the file the manifest
                     names; read from where you run mcpb, and needs --platform
                     when the manifest names several
      --tree <dir>   the files of <dir>, packed into server/ as they are laid out
                     and written as programs - `--tree server` for a directory of
                     that name; relative to the manifest, as its paths are
      --data <dir>   the same, written as files to read

sign:
  Appends a signature to the bundle, patching the zip's end record first so that
  the file stays a valid zip and the signature covers the bytes delivered.

      --cert <file>  the signer's certificate, PEM or DER
      --key <file>   its private key: PKCS#8, encrypted PKCS#8 or PKCS#1 RSA
      --chain <file> a certificate to write into the signature with it, such as
                     an intermediate; may be given more than once
      --force        replace a signature this crate wrote
      --passphrase-file <file>
                     the password of an encrypted key, read from the first line
                     of <file>; otherwise MCPB_PASSPHRASE, otherwise standard
                     input

verify:
  Says whether the bundle's signature covers it and who signed it. Exits 0 only
  for a signature it can trust, or for one whose trust it does not claim to
  establish (`--no-trust`, and Linux without --ca-file).

      --no-trust     check the signature only, not the certificate chain
      --ca-file <file>
                     build the chain from the anchors in <file>, in pure Rust, on
                     any platform; without it macOS and Windows ask their own
                     chain builder and Linux reports that trust was not evaluated
      --allow-self-signed
                     accept a bundle whose signer is its own issuer
      --allow-expired
                     accept a signer certificate that has expired
      --at <time>    check the certificate at <time> (RFC 3339, such as
                     2026-01-31T12:00:00Z) rather than now

prepare-for-signing:
  Reserves a zero-filled slot after the zip for a signature made elsewhere, such
  as by an HSM. The bytes never change afterwards, so what the signer signs is
  what ships.

      --slot <bytes> how much room to reserve, 4096 to 65535 (16384 by default)

apply-signature:
  Fills the reserved slot with a detached DER CMS signature over the bytes before
  it, which is the length prepare-for-signing printed. The signature is checked
  before it is written.

      --signature <file>
                     the DER CMS signature the external signer produced

unsign:
  Removes the signature block a zip's comment declares and restores the bytes that
  were signed. A block the comment does not declare is an error.

validate:
  Checks a manifest against what clients require, or the manifest of a bundle.

info:
  What a bundle holds: its name and version, the platforms and the entry point,
  its size, its signature, and every file with its size.

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
        // A verdict the command reached and printed: the bundle is not what was
        // asked for, so this exits nonzero although nothing failed to run.
        Err(Failure::Rejected(text)) => {
            println!("{text}");
            ExitCode::FAILURE
        }
        Err(Failure::Failed(error)) => {
            eprintln!("mcpb: {error}");
            ExitCode::FAILURE
        }
        Err(Failure::Message(problem)) => {
            eprintln!("mcpb: {problem}");
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

/// Why the command failed: bad arguments, a failed operation, an invalid
/// manifest or bundle, or a verdict that says no.
enum Failure {
    Usage(String),
    Failed(Error),
    /// A failed operation whose message main can complete, such as the path it
    /// tried to write.
    Message(String),
    Invalid {
        what: PathBuf,
        problems: Vec<String>,
    },
    /// The command did what it was asked and the answer is no.
    Rejected(String),
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
    Sign {
        bundle: PathBuf,
        identity: Identity,
        passphrase_file: Option<PathBuf>,
    },
    Unsign(PathBuf),
    Verify(Check),
    Prepare {
        bundle: PathBuf,
        slot: usize,
    },
    Apply {
        bundle: PathBuf,
        signature: PathBuf,
    },
}

/// The certificate, key and chain to sign with.
struct Identity {
    certificate: PathBuf,
    key: PathBuf,
    chain: Vec<PathBuf>,
    force: bool,
}

/// What `verify` was asked to accept.
struct Check {
    bundle: PathBuf,
    ca_file: Option<PathBuf>,
    no_trust: bool,
    allow_self_signed: bool,
    allow_expired: bool,
    at: Option<SystemTime>,
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
        name => match parse(name, rest)? {
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
            Command::Sign {
                bundle,
                identity,
                passphrase_file,
            } => sign(&bundle, &identity, passphrase_file.as_deref()),
            Command::Unsign(bundle) => {
                mcpb::unsign(&bundle)?;
                Ok(Some(format!("unsigned {}", bundle.display())))
            }
            Command::Verify(check) => verify(&check),
            Command::Prepare { bundle, slot } => {
                mcpb::prepare_for_signing(&bundle, slot)?;
                let prepared = fs::metadata(&bundle)
                    .map_err(|source| Error::Read {
                        path: bundle.clone(),
                        source,
                    })?
                    .len();
                Ok(Some(format!(
                    "prepared {} with a {slot}-byte slot; the signature covers its first {} bytes",
                    bundle.display(),
                    prepared - slot as u64
                )))
            }
            Command::Apply { bundle, signature } => {
                mcpb::apply_signature(&bundle, &signature)?;
                Ok(Some(format!("signed {}", bundle.display())))
            }
        },
    }
}

/// An option `mcpb` knows, whichever command it belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flag {
    Out,
    Arch,
    Platform,
    Executable,
    Tree,
    Data,
    Certificate,
    Key,
    Chain,
    Force,
    PassphraseFile,
    NoTrust,
    CaFile,
    AllowSelfSigned,
    AllowExpired,
    At,
    Slot,
    Signature,
}

impl Flag {
    /// The option a command line word names, if any.
    fn of(word: &str) -> Option<Self> {
        match word {
            "-o" | "--out" => Some(Self::Out),
            "--arch" => Some(Self::Arch),
            "--platform" => Some(Self::Platform),
            "--executable" => Some(Self::Executable),
            "--tree" => Some(Self::Tree),
            "--data" => Some(Self::Data),
            "--cert" => Some(Self::Certificate),
            "--key" => Some(Self::Key),
            "--chain" => Some(Self::Chain),
            "--force" => Some(Self::Force),
            "--passphrase-file" => Some(Self::PassphraseFile),
            "--no-trust" => Some(Self::NoTrust),
            "--ca-file" => Some(Self::CaFile),
            "--allow-self-signed" => Some(Self::AllowSelfSigned),
            "--allow-expired" => Some(Self::AllowExpired),
            "--at" => Some(Self::At),
            "--slot" => Some(Self::Slot),
            "--signature" => Some(Self::Signature),
            _ => None,
        }
    }

    /// Whether the option takes a value after it.
    fn takes_value(self) -> bool {
        !matches!(
            self,
            Self::Force | Self::NoTrust | Self::AllowSelfSigned | Self::AllowExpired
        )
    }
}

/// The default room reserved for an external signature.
const DEFAULT_SLOT: usize = 16384;

fn parse(command: &str, args: &[String]) -> Result<Command, Failure> {
    let mut positional: Vec<String> = Vec::new();
    let mut flags: Vec<(Flag, Option<String>, String)> = Vec::new();

    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let flag = match arg.starts_with('-') && arg.len() > 1 {
            true => Flag::of(arg)
                .ok_or_else(|| Failure::Usage(format!("{arg} is no option of mcpb")))?,
            false => {
                positional.push(arg.clone());
                continue;
            }
        };

        let value = match flag.takes_value() {
            true => Some(
                rest.next()
                    .cloned()
                    .ok_or_else(|| Failure::Usage(format!("{arg} needs a value")))?,
            ),
            false => None,
        };
        flags.push((flag, value, arg.clone()));
    }

    let mut out = None;
    let mut arch = None;
    let mut platform = None;
    let mut executable = None;
    let mut trees = Vec::new();
    let mut certificate = None;
    let mut key = None;
    let mut chain = Vec::new();
    let mut force = false;
    let mut passphrase_file = None;
    let mut no_trust = false;
    let mut ca_file = None;
    let mut allow_self_signed = false;
    let mut allow_expired = false;
    let mut at = None;
    let mut slot = DEFAULT_SLOT;
    let mut signature = None;

    for (flag, value, name) in flags {
        let value = value.unwrap_or_default();
        match flag {
            Flag::Out => {
                only(command, "pack", &name)?;
                out = Some(PathBuf::from(value));
            }
            Flag::Arch => {
                only(command, "pack", &name)?;
                arch = Some(value);
            }
            Flag::Platform => {
                only(command, "pack", &name)?;
                platform = Some(Platform::parse(&value).ok_or_else(|| {
                    Failure::Usage(format!(
                        "--platform {value} is none of darwin, linux and win32"
                    ))
                })?);
            }
            Flag::Executable => {
                only(command, "pack", &name)?;
                executable = Some(PathBuf::from(value));
            }
            Flag::Tree => {
                only(command, "pack", &name)?;
                trees.push(Tree::new(value));
            }
            Flag::Data => {
                only(command, "pack", &name)?;
                trees.push(Tree::data(value));
            }
            Flag::Certificate => {
                only(command, "sign", &name)?;
                certificate = Some(PathBuf::from(value));
            }
            Flag::Key => {
                only(command, "sign", &name)?;
                key = Some(PathBuf::from(value));
            }
            Flag::Chain => {
                only(command, "sign", &name)?;
                chain.push(PathBuf::from(value));
            }
            Flag::Force => {
                only(command, "sign", &name)?;
                force = true;
            }
            Flag::PassphraseFile => {
                only(command, "sign", &name)?;
                passphrase_file = Some(PathBuf::from(value));
            }
            Flag::NoTrust => {
                only(command, "verify", &name)?;
                no_trust = true;
            }
            Flag::CaFile => {
                only(command, "verify", &name)?;
                ca_file = Some(PathBuf::from(value));
            }
            Flag::AllowSelfSigned => {
                only(command, "verify", &name)?;
                allow_self_signed = true;
            }
            Flag::AllowExpired => {
                only(command, "verify", &name)?;
                allow_expired = true;
            }
            Flag::At => {
                only(command, "verify", &name)?;
                at = Some(instant(&value).ok_or_else(|| {
                    Failure::Usage(format!(
                        "--at {value} is no time; write 2026-01-31T12:00:00Z or a Unix timestamp"
                    ))
                })?);
            }
            Flag::Slot => {
                only(command, "prepare-for-signing", &name)?;
                slot = value
                    .parse()
                    .map_err(|_| Failure::Usage(format!("--slot {value} is no number of bytes")))?;
            }
            Flag::Signature => {
                only(command, "apply-signature", &name)?;
                signature = Some(PathBuf::from(value));
            }
        }
    }

    if no_trust && ca_file.is_some() {
        return Err(Failure::Usage(
            "--no-trust and --ca-file ask for opposite things".to_string(),
        ));
    }

    let command = match command {
        "pack" => Command::Pack {
            manifest: one(command, positional)?,
            out,
            arch,
            platform,
            executable,
            trees,
        },
        "sign" => Command::Sign {
            bundle: one(command, positional)?,
            identity: Identity {
                certificate: certificate
                    .ok_or_else(|| Failure::Usage("sign needs --cert".to_string()))?,
                key: key.ok_or_else(|| Failure::Usage("sign needs --key".to_string()))?,
                chain,
                force,
            },
            passphrase_file,
        },
        "unsign" => Command::Unsign(one(command, positional)?),
        "verify" => Command::Verify(Check {
            bundle: one(command, positional)?,
            ca_file,
            no_trust,
            allow_self_signed,
            allow_expired,
            at,
        }),
        "prepare-for-signing" => Command::Prepare {
            bundle: one(command, positional)?,
            slot,
        },
        "apply-signature" => Command::Apply {
            bundle: one(command, positional)?,
            signature: signature
                .ok_or_else(|| Failure::Usage("apply-signature needs --signature".to_string()))?,
        },
        "validate" | "info" => {
            let path = one(command, positional)?;
            match command {
                "validate" => Command::Validate(path),
                _ => Command::Info(path),
            }
        }
        other => {
            return Err(Failure::Usage(format!("{other} is no command of mcpb")));
        }
    };

    Ok(command)
}

/// Refuses an option that belongs to another command.
fn only(command: &str, belongs: &str, flag: &str) -> Result<(), Failure> {
    match command {
        _ if command == belongs => Ok(()),
        _ => Err(Failure::Usage(format!("{flag} is no option of {command}"))),
    }
}

/// The one file a command names.
fn one(command: &str, positional: Vec<String>) -> Result<PathBuf, Failure> {
    let mut paths = positional.into_iter();
    let path = paths
        .next()
        .ok_or_else(|| Failure::Usage(format!("{command} needs a bundle or a manifest")))?;
    if let Some(extra) = paths.next() {
        return Err(Failure::Usage(format!(
            "{command} takes one file, not {path} and {extra}"
        )));
    }
    Ok(PathBuf::from(path))
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
            // A binary `cargo` built is named from where the command runs, as
            // `-o` is, and not from where the manifest lives: in a workspace
            // the manifest is in a member and `target/` is at the root.
            (Some(only), None) => only.path = executable,
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
        Some(dir) if is_directory(&dir) => {
            fs::create_dir_all(&dir).map_err(|error| {
                Failure::Message(format!("can't create {}: {error}", dir.display()))
            })?;
            dir.join(spec.file_name())
        }
        Some(out) => out,
        None => PathBuf::from(spec.file_name()),
    };
    if let Err(error) = spec.pack(&out) {
        return Err(match error {
            Error::Write { source } => {
                Failure::Message(format!("can't write {}: {source}", out.display()))
            }
            error => Failure::Failed(error),
        });
    }

    let bundle = Bundle::open(&out)?;
    Ok(format!(
        "packed {} ({} files, {}, {})",
        out.display(),
        bundle.entries().len(),
        size(bundle.size()),
        match bundle.has_signature_block() {
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

/// Whether `path` names a directory to write into: one that exists, or one
/// whose name ends with a separator, as `dist/` does.
fn is_directory(path: &Path) -> bool {
    path.is_dir()
        || path
            .as_os_str()
            .to_string_lossy()
            .chars()
            .next_back()
            .is_some_and(std::path::is_separator)
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

    let at = SystemTime::now();
    match bundle.signature_at(at)? {
        SignatureState::None => text.push_str("\n  signed: no"),
        SignatureState::Foreign(layout) => {
            text.push_str(&format!(
                "\n  signed: in a layout this crate does not verify ({layout})"
            ));
        }
        SignatureState::Present(signature) => {
            // A report never fails: a bad signature is one of the things it is
            // for, and the trust it could not establish is printed with it.
            let trust = mcpb::check_trust(&signature, at, None);
            text.push_str(&format!("\n  signed: {}", state(&signature)));
            text.push_str(&details(&signature, &trust));
        }
    }

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

/// Signs the bundle at `bundle` with the identity the caller named.
fn sign(
    bundle: &Path,
    identity: &Identity,
    passphrase_file: Option<&Path>,
) -> Result<Option<String>, Failure> {
    let key = SigningKey::load(
        &identity.certificate,
        &identity.key,
        &identity.chain,
        || passphrase(passphrase_file),
    )?;

    mcpb::sign_with(
        bundle,
        &key,
        &SignOptions {
            force: identity.force,
            signing_time: None,
        },
    )?;

    Ok(Some(format!("signed {}", bundle.display())))
}

/// The passphrase of an encrypted key, never from the command line.
fn passphrase(file: Option<&Path>) -> Result<String, Error> {
    if let Some(file) = file {
        let text = fs::read_to_string(file).map_err(|source| Error::Read {
            path: file.to_path_buf(),
            source,
        })?;
        return Ok(text.trim_end_matches(['\n', '\r']).to_string());
    }

    if let Ok(value) = std::env::var("MCPB_PASSPHRASE") {
        return Ok(value);
    }

    eprintln!("mcpb: the private key is encrypted; reading its passphrase from standard input");
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|source| Error::Read {
            path: PathBuf::from("standard input"),
            source,
        })?;
    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

/// Says whether a bundle's signature can be trusted, and exits accordingly.
fn verify(check: &Check) -> Result<Option<String>, Failure> {
    let at = check.at.unwrap_or_else(SystemTime::now);
    let state = mcpb::signature_at(&check.bundle, at)?;
    let path = check.bundle.display();

    let signature = match &state {
        SignatureState::None => {
            return Err(Failure::Rejected(format!(
                "{path}: unsigned, it has no signature block"
            )));
        }
        SignatureState::Foreign(layout) => {
            return Err(Failure::Rejected(format!(
                "{path}: unsupported signature, {layout}"
            )));
        }
        SignatureState::Present(signature) => signature,
    };

    match &signature.crypto {
        CryptoValidity::Valid => {}
        CryptoValidity::Invalid(why) => {
            return Err(Failure::Rejected(format!(
                "{path}: invalid signature: {why}"
            )));
        }
        CryptoValidity::Malformed(why) => {
            return Err(Failure::Rejected(format!(
                "{path}: unreadable signature: {why}"
            )));
        }
        CryptoValidity::Unsupported(what) => {
            return Err(Failure::Rejected(format!(
                "{path}: unsupported signature: {what}"
            )));
        }
    }

    let Some(signer) = &signature.signer else {
        return Err(Failure::Rejected(format!(
            "{path}: the signature names no signer"
        )));
    };

    if matches!(
        signature.validity,
        CertificateValidity::Expired | CertificateValidity::NotYetValid
    ) && !check.allow_expired
    {
        return Err(Failure::Rejected(format!(
            "{path}: signed, but the signer certificate is {}; --allow-expired accepts it anyway{}",
            signature.validity,
            details(signature, &Trust::NotEvaluated)
        )));
    }

    let trust = match check.no_trust {
        true => Trust::NotEvaluated,
        false => mcpb::check_trust(signature, at, check.ca_file.as_deref()),
    };

    let accepted = if check.no_trust || (signer.self_signed && check.allow_self_signed) {
        true
    } else {
        match &trust {
            Trust::Trusted => true,
            // Linux has no code-signing policy store, so a CA-issued signer is
            // neither trusted nor refused there; a self-signed one still is.
            Trust::NotEvaluated => !signer.self_signed,
            Trust::Untrusted(_) | Trust::Error(_) => false,
        }
    };

    let text = match accepted {
        true => {
            let mut text = format!(
                "{path}: signed by {}{}",
                signer.subject,
                details(signature, &trust)
            );
            // Linux has no policy store to ask, so a plain `verify` there says
            // who signed a bundle and not who they are, and has to say so.
            if !check.no_trust && matches!(trust, Trust::NotEvaluated) {
                text.push_str(
                    "\n  note: no certificate authority was checked, so this names a signer, not a publisher",
                );
            }
            text
        }
        false => {
            let why = match (&trust, signer.self_signed) {
                (Trust::NotEvaluated, true) => {
                    "it is self-signed, so it names no publisher; --allow-self-signed accepts it anyway"
                        .to_string()
                }
                _ => trust.to_string(),
            };
            format!("{path}: signed, but {why}{}", details(signature, &trust))
        }
    };

    match accepted {
        true => Ok(Some(text)),
        false => Err(Failure::Rejected(text)),
    }
}

/// A signature's state in a few words.
fn state(signature: &Signature) -> String {
    let layout = match &signature.layout {
        Some(layout) => layout.to_string(),
        None => "unreadable".to_string(),
    };
    let crypto = match &signature.crypto {
        CryptoValidity::Valid => "signature valid".to_string(),
        CryptoValidity::Invalid(why) => format!("invalid signature: {why}"),
        CryptoValidity::Malformed(why) => format!("unreadable signature: {why}"),
        CryptoValidity::Unsupported(what) => format!("unsupported: {what}"),
    };

    format!(
        "yes ({layout}, {crypto}, certificate {})",
        signature.validity
    )
}

/// The lines a bundle's signature is described with.
fn details(signature: &Signature, trust: &Trust) -> String {
    let mut text = String::new();
    if let Some(signer) = &signature.signer {
        text.push_str(&format!("\n  publisher: {}", signer.subject));
        text.push_str(&format!("\n  issuer: {}", signer.issuer));
        text.push_str(&format!(
            "\n  valid: {} to {} ({}{})",
            moment(signer.valid_from),
            moment(signer.valid_to),
            signature.validity,
            match signer.self_signed {
                true => ", self-signed",
                false => "",
            }
        ));
        text.push_str(&format!("\n  sha256: {}", signer.sha256));
    }
    if let Some(at) = signature.signed_at {
        text.push_str(&format!(
            "\n  signed at: {} (claimed by the signer)",
            moment(at)
        ));
    }
    text.push_str(&format!("\n  trust: {trust}"));
    text
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

/// The instant `text` names: an RFC 3339 timestamp in UTC, or seconds since the
/// Unix epoch.
fn instant(text: &str) -> Option<SystemTime> {
    if let Ok(seconds) = text.parse::<u64>() {
        return SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds));
    }

    let text = text
        .strip_suffix('Z')
        .or_else(|| text.strip_suffix("+00:00"))?;
    let (date, clock) = text.split_once(['T', ' '])?;

    let mut date = date.split('-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: u32 = date.next()?.parse().ok()?;
    let day: u32 = date.next()?.parse().ok()?;
    if date.next().is_some() {
        return None;
    }

    let mut clock = clock.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    // A fractional second names no whole second, so it is dropped.
    let second: i64 = clock.next()?.split('.').next()?.parse().ok()?;
    if clock.next().is_some()
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..24).contains(&hour)
        || !(0..60).contains(&minute)
        || !(0..61).contains(&second)
    {
        return None;
    }

    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;
    match seconds.is_negative() {
        true => SystemTime::UNIX_EPOCH.checked_sub(Duration::from_secs(seconds.unsigned_abs())),
        false => SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds as u64)),
    }
}

/// `at` as an RFC 3339 timestamp in UTC.
fn moment(at: SystemTime) -> String {
    let seconds = match at.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(since) => since.as_secs() as i64,
        Err(before) => -(before.duration().as_secs() as i64),
    };
    let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
    let clock = seconds.rem_euclid(86_400);

    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        clock / 3600,
        (clock % 3600) / 60,
        clock % 60
    )
}

/// Days from 1970-01-01 to a civil date, and back again, as in Howard Hinnant's
/// `days_from_civil` and `civil_from_days`.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = match month <= 2 {
        true => year - 1,
        false => year,
    };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year =
        (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;

    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month + 2) / 5 + 1;
    let month = match month < 10 {
        true => month + 3,
        false => month - 9,
    };

    (year + i64::from(month <= 2), month as u32, day as u32)
}
