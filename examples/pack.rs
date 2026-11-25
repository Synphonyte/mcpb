//! Packs a bundle for this platform, from the example binary itself or from an
//! executable you point it at:
//!
//! ```sh
//! cargo run --example pack
//! cargo run --example pack -- --executable target/release/my-server --platform linux --arch arm64
//! ```
//!
//! It writes `target/bundles/<name>-<version>-<platform>-<arch>.mcpb` and prints
//! the path, which is what CI checks with the `mcpb` CLI's schema validation.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use mcpb::{Author, BundleSpec, Executable, Platform, UserConfig};

fn main() -> ExitCode {
    match pack() {
        Ok(path) => {
            println!("{}", path.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn pack() -> Result<PathBuf, String> {
    let mut executable = None;
    let mut platform = Platform::host();
    let mut arch = env::consts::ARCH.to_string();

    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--executable" => executable = Some(next(&mut arguments, &argument)?),
            "--platform" => {
                let name = next(&mut arguments, &argument)?;
                platform = Platform::parse(&name)
                    .ok_or_else(|| format!("{name:?} is not a platform, such as darwin"))?;
            }
            "--arch" => arch = next(&mut arguments, &argument)?,
            other => return Err(format!("unknown argument {other:?}")),
        }
    }

    let executable = match executable {
        Some(path) => PathBuf::from(path),
        None => env::current_exe().map_err(|error| format!("Can't find this binary: {error}"))?,
    };

    let spec = BundleSpec {
        name: "mcpb-example".to_string(),
        display_name: Some("mcpb example".to_string()),
        version: env!("CARGO_PKG_VERSION")
            .parse()
            .expect("the crate's version is a semantic version"),
        description: "A bundle built by the mcpb crate's own example.".to_string(),
        author: Author::new("Synphonyte"),
        executables: BTreeMap::from([(platform, Executable::new(executable, arch.clone()))]),
        tools: vec![("greet".to_string(), "Say hello to a name.".to_string())],
        user_config: vec![UserConfig {
            required: true,
            ..UserConfig::string("name", "Your name", "Whom the server greets.")
        }],
        args: vec!["--greet".to_string(), "${user_config.name}".to_string()],
        env: BTreeMap::new(),
        ..BundleSpec::default()
    };

    let directory = PathBuf::from("target/bundles");
    fs::create_dir_all(&directory)
        .map_err(|error| format!("Can't create {}: {error}", directory.display()))?;
    let bundle = directory.join(spec.file_name());
    spec.pack(&bundle).map_err(|error| error.to_string())?;

    Ok(bundle)
}

fn next(arguments: &mut impl Iterator<Item = String>, argument: &str) -> Result<String, String> {
    arguments
        .next()
        .ok_or_else(|| format!("{argument} needs a value"))
}
