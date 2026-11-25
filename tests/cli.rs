//! The `mcpb` binary, run as a person runs it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The manifest of the spec's own Rust example, with its binary beside it.
fn example(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir.join("server")).expect("the directory");
    fs::write(
        dir.join("manifest.json"),
        include_str!("fixtures/calculator-rust.manifest.json"),
    )
    .expect("the manifest");
    fs::write(dir.join("server/mcp-calculator"), "\x7fELF fake").expect("the binary");

    dir.to_path_buf()
}

fn mcpb(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mcpb"))
        .args(args)
        .output()
        .expect("the binary runs")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn it_validates_a_directory_a_manifest_and_a_bundle() {
    let dir = tempfile::TempDir::new().expect("a temporary directory");
    let example = example(&dir.path().join("example"));

    for target in [example.clone(), example.join("manifest.json"), {
        let bundle = dir.path().join("out.mcpb");
        let output = mcpb(&[
            "pack",
            example.to_str().expect("UTF-8"),
            "-o",
            bundle.to_str().expect("UTF-8"),
        ]);
        assert!(output.status.success(), "{}", text(&output));
        bundle
    }] {
        let output = mcpb(&["validate", target.to_str().expect("UTF-8")]);
        assert!(output.status.success(), "{}", text(&output));
        assert!(
            text(&output).contains("calculator-rust 1.0.0 on darwin, linux"),
            "{}",
            text(&output)
        );
    }
}

#[test]
fn it_packs_and_reads_back_what_it_packed() {
    let dir = tempfile::TempDir::new().expect("a temporary directory");
    let example = example(&dir.path().join("example"));
    let bundle = dir.path().join("dist").join("calculator.mcpb");
    fs::create_dir_all(bundle.parent().expect("a parent")).expect("the directory");

    let output = mcpb(&[
        "pack",
        example.to_str().expect("UTF-8"),
        "-o",
        bundle.to_str().expect("UTF-8"),
        "--arch",
        "x64",
        "--tree",
        "server",
    ]);
    assert!(output.status.success(), "{}", text(&output));
    assert!(text(&output).contains("packed"), "{}", text(&output));

    let output = mcpb(&["info", bundle.to_str().expect("UTF-8")]);
    assert!(output.status.success(), "{}", text(&output));
    let info = text(&output);
    for expected in [
        "calculator-rust 1.0.0",
        "manifest version: 0.3",
        "platforms: darwin, linux",
        "entry point: server/mcp-calculator",
        "signed: no",
        "server/mcp-calculator *",
    ] {
        assert!(info.contains(expected), "{expected} is not in:\n{info}");
    }
}

#[test]
fn a_spec_that_clients_would_refuse_is_reported() {
    let dir = tempfile::TempDir::new().expect("a temporary directory");
    let example = example(&dir.path().join("example"));
    let manifest = example.join("manifest.json");
    let mut spec: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&manifest).expect("the manifest")).expect("JSON");
    spec["description"] = "".into();
    fs::write(&manifest, spec.to_string()).expect("the manifest");

    let output = mcpb(&["validate", example.to_str().expect("UTF-8")]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output).contains("description is empty"),
        "{}",
        text(&output)
    );
    assert!(text(&output).contains("is not valid"), "{}", text(&output));
    assert!(!text(&output).contains("packed"), "{}", text(&output));
}

#[test]
fn a_bundle_without_the_files_its_manifest_names_is_reported() {
    use std::io::Write;

    let dir = tempfile::TempDir::new().expect("a temporary directory");
    let bundle = dir.path().join("hollow.mcpb");
    let mut zip = zip::ZipWriter::new(fs::File::create(&bundle).expect("the bundle"));
    zip.start_file("manifest.json", zip::write::SimpleFileOptions::default())
        .expect("an entry");
    zip.write_all(include_bytes!("fixtures/calculator-rust.manifest.json"))
        .expect("the manifest");
    zip.finish().expect("the zip");

    let output = mcpb(&["validate", bundle.to_str().expect("UTF-8")]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        text(&output).contains("server.entry_point names server/mcp-calculator"),
        "{}",
        text(&output)
    );
}

#[test]
fn a_command_it_does_not_have_says_so() {
    let output = mcpb(&[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output).contains("no command"), "{}", text(&output));

    let output = mcpb(&["frobnicate"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        text(&output).contains("frobnicate is no command of mcpb"),
        "{}",
        text(&output)
    );

    let output = mcpb(&["--help"]);
    assert!(output.status.success());
    assert!(text(&output).contains("mcpb pack"), "{}", text(&output));
}
