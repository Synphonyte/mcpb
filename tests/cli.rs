//! The `mcpb` binary, run end to end.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The upstream Rust example's manifest, with its binary beside it.
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

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/signing")
        .join(name)
}

/// A packed bundle, and the directory it lives in.
fn packed(dir: &Path) -> PathBuf {
    let example = example(&dir.join("example"));
    let bundle = dir.join("out.mcpb");
    let output = mcpb(&[
        "pack",
        example.to_str().expect("UTF-8"),
        "-o",
        bundle.to_str().expect("UTF-8"),
    ]);
    assert!(output.status.success(), "{}", text(&output));
    bundle
}

/// Signs `bundle` with the test identity named `certificate` and `key`.
fn sign(bundle: &Path, certificate: &str, key: &str) -> Output {
    mcpb(&[
        "sign",
        bundle.to_str().expect("UTF-8"),
        "--cert",
        fixture(certificate).to_str().expect("UTF-8"),
        "--key",
        fixture(key).to_str().expect("UTF-8"),
    ])
}

/// Runs `verify` on `bundle` with `flags` after it.
fn verify(bundle: &Path, flags: &[&str]) -> Output {
    let mut args = vec!["verify", bundle.to_str().expect("UTF-8")];
    args.extend_from_slice(flags);
    mcpb(&args)
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

#[test]
fn it_packs_one_platform_with_a_binary_from_where_it_runs() {
    let dir = tempfile::TempDir::new().expect("a temporary directory");
    let example = example(&dir.path().join("example"));
    // Where cargo puts the binary - beside the workspace, not the member whose
    // manifest is packed - as it is named from where the command runs.
    let built = dir.path().join("target/release/calculator");
    fs::create_dir_all(built.parent().expect("a parent")).expect("the directory");
    fs::write(&built, "\x7fELF built by cargo").expect("the binary");
    fs::remove_file(example.join("server/mcp-calculator")).expect("removed");

    let bundle = dir.path().join("linux.mcpb");
    let output = Command::new(env!("CARGO_BIN_EXE_mcpb"))
        .args(["pack", "example", "--platform", "linux", "--arch", "x64"])
        .args(["--executable", "target/release/calculator"])
        .args(["-o", bundle.to_str().expect("UTF-8")])
        .current_dir(dir.path())
        .output()
        .expect("the binary runs");
    assert!(output.status.success(), "{}", text(&output));

    let bundle = mcpb::Bundle::open(&bundle).expect("the bundle");
    assert_eq!(
        bundle.manifest()["compatibility"]["platforms"],
        serde_json::json!(["linux"])
    );
    assert_eq!(bundle.entry_point(), Some("server/mcp-calculator"));
    let entry = bundle
        .entries()
        .iter()
        .find(|entry| entry.name == "server/mcp-calculator")
        .expect("the binary is packed");
    assert_eq!(entry.size, "\x7fELF built by cargo".len() as u64);
    assert!(entry.executable);
}

#[test]
fn it_names_a_bundle_after_the_one_platform() {
    let dir = tempfile::TempDir::new().expect("a temporary directory");
    example(&dir.path().join("example"));

    let output = Command::new(env!("CARGO_BIN_EXE_mcpb"))
        .args(["pack", "example", "--platform", "darwin", "--arch", "arm64"])
        .current_dir(dir.path())
        .output()
        .expect("the binary runs");
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        dir.path()
            .join("calculator-rust-1.0.0-darwin-arm64.mcpb")
            .is_file(),
        "{}",
        text(&output)
    );

    // A directory to write to is named into, and one that ends with a
    // separator is made rather than mistaken for a file called `dist`.
    let output = Command::new(env!("CARGO_BIN_EXE_mcpb"))
        .args(["pack", "example", "--platform", "linux", "--arch", "x64"])
        .args(["-o", "dist/"])
        .current_dir(dir.path())
        .output()
        .expect("the binary runs");
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        dir.path()
            .join("dist/calculator-rust-1.0.0-linux-x64.mcpb")
            .is_file(),
        "{}",
        text(&output)
    );
}

#[test]
fn a_bundle_it_cannot_write_names_the_path() {
    let dir = tempfile::TempDir::new().expect("a temporary directory");
    let example = example(&dir.path().join("example"));
    let bundle = dir.path().join("missing/bundle.mcpb");

    let output = mcpb(&[
        "pack",
        example.to_str().expect("UTF-8"),
        "-o",
        bundle.to_str().expect("UTF-8"),
    ]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        text(&output).contains(&format!("can't write {}", bundle.display())),
        "{}",
        text(&output)
    );
}

#[test]
fn a_platform_or_executable_it_cannot_use_says_so() {
    let dir = tempfile::TempDir::new().expect("a temporary directory");
    let example = example(&dir.path().join("example"));
    let example = example.to_str().expect("UTF-8");

    // The manifest names darwin and linux.
    let output = mcpb(&["pack", example, "--platform", "win32"]);
    assert_eq!(output.status.code(), Some(2), "{}", text(&output));
    assert!(
        text(&output).contains("names no such platform"),
        "{}",
        text(&output)
    );

    let output = mcpb(&["pack", example, "--platform", "windows"]);
    assert_eq!(output.status.code(), Some(2), "{}", text(&output));
    assert!(
        text(&output).contains("none of darwin, linux and win32"),
        "{}",
        text(&output)
    );

    let output = mcpb(&["pack", example, "--executable", "server/mcp-calculator"]);
    assert_eq!(output.status.code(), Some(2), "{}", text(&output));
    assert!(
        text(&output).contains("--executable needs --platform"),
        "{}",
        text(&output)
    );
}

#[test]
fn it_signs_verifies_and_unsigns_a_bundle() {
    let dir = tempfile::TempDir::new().expect("a temporary directory");
    let bundle = packed(dir.path());
    let unsigned = fs::read(&bundle).expect("the bundle");

    let output = mcpb(&["verify", bundle.to_str().expect("UTF-8")]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("unsigned"), "{}", text(&output));

    let output = sign(&bundle, "leaf.pem", "leaf.key");
    assert!(output.status.success(), "{}", text(&output));

    let output = verify(
        &bundle,
        &["--ca-file", fixture("ca.pem").to_str().expect("UTF-8")],
    );
    assert!(output.status.success(), "{}", text(&output));
    let report = text(&output);
    for expected in [
        "signed by CN=mcpb test signer",
        "publisher: CN=mcpb test signer",
        "issuer: CN=mcpb test CA",
        "trust: trusted",
        "sha256: ",
    ] {
        assert!(report.contains(expected), "{expected} is not in:\n{report}");
    }

    // `info` says the same without deciding anything.
    let output = mcpb(&["info", bundle.to_str().expect("UTF-8")]);
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        text(&output).contains("signature valid"),
        "{}",
        text(&output)
    );

    let output = mcpb(&["unsign", bundle.to_str().expect("UTF-8")]);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(fs::read(&bundle).expect("the bundle"), unsigned);
}

#[test]
fn a_signature_it_cannot_trust_is_not_accepted() {
    let dir = tempfile::TempDir::new().expect("a temporary directory");
    let bundle = packed(dir.path());
    assert!(sign(&bundle, "leaf.pem", "leaf.key").status.success());

    // A certificate authority that did not sign the bundle.
    let output = verify(
        &bundle,
        &["--ca-file", fixture("ec-ca.pem").to_str().expect("UTF-8")],
    );
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("untrusted"), "{}", text(&output));

    // Without a chain check the signature alone is enough.
    let output = verify(&bundle, &["--no-trust"]);
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        text(&output).contains("trust: not evaluated"),
        "{}",
        text(&output)
    );

    // A certificate is only valid while it is valid.
    let output = verify(&bundle, &["--no-trust", "--at", "1999-06-01T00:00:00Z"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("not yet valid"), "{}", text(&output));

    let output = verify(
        &bundle,
        &[
            "--no-trust",
            "--at",
            "1999-06-01T00:00:00Z",
            "--allow-expired",
        ],
    );
    assert!(output.status.success(), "{}", text(&output));

    // And the two ways of asking about trust cannot be asked together.
    let output = verify(
        &bundle,
        &[
            "--no-trust",
            "--ca-file",
            fixture("ca.pem").to_str().expect("UTF-8"),
        ],
    );
    assert_eq!(output.status.code(), Some(2), "{}", text(&output));
    assert!(
        text(&output).contains("opposite things"),
        "{}",
        text(&output)
    );
}

#[test]
fn a_self_signed_bundle_needs_to_be_allowed() {
    let dir = tempfile::TempDir::new().expect("a temporary directory");
    let bundle = packed(dir.path());
    let output = sign(&bundle, "self-signed.pem", "self-signed.key");
    assert!(output.status.success(), "{}", text(&output));

    let output = verify(&bundle, &[]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));

    let output = verify(&bundle, &["--allow-self-signed"]);
    assert!(output.status.success(), "{}", text(&output));
    assert!(text(&output).contains("self-signed"), "{}", text(&output));

    let output = verify(&bundle, &["--no-trust"]);
    assert!(output.status.success(), "{}", text(&output));

    // Naming the certificate itself as the anchor is not consent to a self-signed
    // signer: it is still the certificate speaking for itself.
    let ca = fixture("self-signed.pem");
    let output = verify(&bundle, &["--ca-file", ca.to_str().expect("UTF-8")]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        text(&output).contains("--allow-self-signed"),
        "{}",
        text(&output)
    );

    let output = verify(
        &bundle,
        &[
            "--ca-file",
            ca.to_str().expect("UTF-8"),
            "--allow-self-signed",
        ],
    );
    assert!(output.status.success(), "{}", text(&output));
}

#[test]
fn an_expired_signer_is_only_refused_while_the_chain_is_checked() {
    let dir = tempfile::TempDir::new().expect("a temporary directory");
    let bundle = packed(dir.path());

    // `chained` is signed by an intermediate, so the chain is complete only when
    // the signature carries it.
    let output = mcpb(&[
        "sign",
        bundle.to_str().expect("UTF-8"),
        "--cert",
        fixture("chained.pem").to_str().expect("UTF-8"),
        "--key",
        fixture("chained.key").to_str().expect("UTF-8"),
        "--chain",
        fixture("intermediate.pem").to_str().expect("UTF-8"),
    ]);
    assert!(output.status.success(), "{}", text(&output));

    let ca = fixture("ca.pem");
    let ca = ca.to_str().expect("UTF-8");
    let output = verify(&bundle, &["--ca-file", ca]);
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        text(&output).contains("trust: trusted"),
        "{}",
        text(&output)
    );

    // Past the signer's own expiry the chain is refused, and `--allow-expired`
    // downgrades that same condition rather than leaving it to the chain check.
    let output = verify(&bundle, &["--ca-file", ca, "--at", "2101-01-01T00:00:00Z"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("expired"), "{}", text(&output));

    let output = verify(
        &bundle,
        &[
            "--ca-file",
            ca,
            "--at",
            "2101-01-01T00:00:00Z",
            "--allow-expired",
        ],
    );
    assert!(output.status.success(), "{}", text(&output));
}

#[test]
fn signing_refuses_what_it_cannot_sign_with() {
    let dir = tempfile::TempDir::new().expect("a temporary directory");
    let bundle = packed(dir.path());
    let unsigned = fs::read(&bundle).expect("the bundle");

    for (certificate, key, expected) in [
        ("no-eku.pem", "no-eku.key", "codeSigning"),
        ("weak.pem", "weak.key", "at least 2048"),
        ("expired.pem", "expired.key", "expired"),
        ("leaf.pem", "other.key", "not the certificate's"),
    ] {
        let output = sign(&bundle, certificate, key);
        assert_eq!(output.status.code(), Some(1), "{certificate}");
        assert!(
            text(&output).contains(expected),
            "{certificate} said:\n{}",
            text(&output)
        );
        assert_eq!(fs::read(&bundle).expect("the bundle"), unsigned);
    }

    // Signing twice is refused, and --force replaces.
    assert!(sign(&bundle, "leaf.pem", "leaf.key").status.success());
    let output = sign(&bundle, "leaf.pem", "leaf.key");
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("--force"), "{}", text(&output));

    let output = mcpb(&[
        "sign",
        bundle.to_str().expect("UTF-8"),
        "--cert",
        fixture("leaf.pem").to_str().expect("UTF-8"),
        "--key",
        fixture("leaf.key").to_str().expect("UTF-8"),
        "--chain",
        fixture("intermediate.pem").to_str().expect("UTF-8"),
        "--force",
    ]);
    assert!(output.status.success(), "{}", text(&output));
}

#[test]
fn an_upstream_layout_is_reported_as_unsupported() {
    for name in ["upstream-212.mcpb", "upstream-main.mcpb"] {
        let output = verify(&fixture(name), &["--no-trust"]);
        assert_eq!(output.status.code(), Some(1), "{name}");
        assert!(
            text(&output).contains("unsupported signature"),
            "{name}: {}",
            text(&output)
        );
        assert!(
            !text(&output).contains("unsigned"),
            "{name}: {}",
            text(&output)
        );
    }
}

#[test]
fn an_external_signer_fills_a_reserved_slot() {
    // The pipeline is only complete with a signature from outside, and `openssl`
    // is the external signer every machine with a shell has.
    let available = Command::new("openssl")
        .arg("version")
        .output()
        .is_ok_and(|output| output.status.success());

    let dir = tempfile::TempDir::new().expect("a temporary directory");
    let bundle = packed(dir.path());

    let output = mcpb(&["prepare-for-signing", bundle.to_str().expect("UTF-8")]);
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        text(&output).contains("16384-byte slot"),
        "{}",
        text(&output)
    );

    // Preparing twice would move the bytes an external signer already signed.
    let output = mcpb(&["prepare-for-signing", bundle.to_str().expect("UTF-8")]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));

    if !available {
        eprintln!("skipped the signature itself: openssl is not installed");
        return;
    }

    // What an external signer signs is the prepared file without its slot: the
    // bytes the signature covers, and the bytes that ship.
    let prepared = fs::read(&bundle).expect("the prepared bundle");
    let content = dir.path().join("content");
    fs::write(&content, &prepared[..prepared.len() - 16384]).expect("the content");

    let signature = dir.path().join("sig.der");
    let output = Command::new("openssl")
        .args([
            "cms", "-sign", "-binary", "-md", "sha256", "-outform", "DER",
        ])
        .arg("-signer")
        .arg(fixture("leaf.pem"))
        .arg("-inkey")
        .arg(fixture("leaf.key"))
        .arg("-in")
        .arg(&content)
        .arg("-out")
        .arg(&signature)
        .output()
        .expect("openssl runs");
    assert!(output.status.success(), "{}", text(&output));

    let output = mcpb(&[
        "apply-signature",
        bundle.to_str().expect("UTF-8"),
        "--signature",
        signature.to_str().expect("UTF-8"),
    ]);
    assert!(output.status.success(), "{}", text(&output));

    let output = verify(
        &bundle,
        &["--ca-file", fixture("ca.pem").to_str().expect("UTF-8")],
    );
    assert!(output.status.success(), "{}", text(&output));
}

#[test]
fn openssl_accepts_the_signature_it_writes() {
    // Another implementation has to read what this one writes, or the format is
    // only self-consistent.
    let available = Command::new("openssl")
        .arg("version")
        .output()
        .is_ok_and(|output| output.status.success());
    if !available {
        eprintln!("skipped: openssl is not installed");
        return;
    }

    let dir = tempfile::TempDir::new().expect("a temporary directory");
    let bundle = packed(dir.path());
    assert!(sign(&bundle, "leaf.pem", "leaf.key").status.success());

    let signed = fs::read(&bundle).expect("the bundle");
    let header = signed
        .windows(11)
        .rposition(|window| window == b"MCPB_SIG_V1")
        .expect("the block");
    let length = u32::from_le_bytes(
        signed[header + 11..header + 15]
            .try_into()
            .expect("four bytes"),
    ) as usize;

    let signer = dir.path().join("sig.der");
    let content = dir.path().join("content");
    let sink = dir.path().join("verified");
    fs::write(&signer, &signed[header + 15..header + 15 + length]).expect("the signature");
    fs::write(&content, &signed[..header]).expect("the signed bytes");

    let output = Command::new("openssl")
        .args(["cms", "-verify", "-binary", "-inform", "DER", "-noverify"])
        .arg("-in")
        .arg(&signer)
        .arg("-content")
        .arg(&content)
        .arg("-out")
        .arg(&sink)
        .output()
        .expect("openssl runs");
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(
        fs::read(&sink).expect("the verified content"),
        signed[..header].to_vec()
    );
}
