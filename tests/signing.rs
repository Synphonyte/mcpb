//! Signing, verifying and unsigning bundles, and the layouts this crate refuses.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use mcpb::{
    Author, Bundle, BundleSpec, CertificateValidity, CryptoValidity, Executable, ForeignLayout,
    Layout, Platform, SignOptions, Signature, SignatureState, SigningKey, Trust, TrustProblem,
    apply_signature, check_trust, prepare_for_signing, sign, sign_with, unsign,
};

/// The test identities. They are test data: their keys are published on purpose.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/signing")
        .join(name)
}

/// An identity, with the certificates to carry in its signature.
fn identity(certificate: &str, key: &str, chain: &[&str]) -> SigningKey {
    let chain: Vec<PathBuf> = chain.iter().map(|name| fixture(name)).collect();
    SigningKey::load(&fixture(certificate), &fixture(key), &chain, || {
        Ok(String::new())
    })
    .unwrap_or_else(|error| panic!("{certificate} with {key}: {error}"))
}

/// The identity most tests sign with.
fn signer() -> SigningKey {
    identity("leaf.pem", "leaf.key", &[])
}

/// A bundle of one Linux binary, packed into `dir`.
fn bundle_in(dir: &Path) -> PathBuf {
    let binary = dir.join("server-binary");
    fs::write(&binary, b"\x7fELF fake").unwrap();

    let spec = BundleSpec {
        name: "signed-server".to_string(),
        version: "1.0.0".parse().unwrap(),
        description: "A server to sign.".to_string(),
        author: Author::new("Test"),
        executables: BTreeMap::from([(Platform::Linux, Executable::new(&binary, "x64"))]),
        ..BundleSpec::default()
    };

    let path = dir.join("signed.mcpb");
    spec.pack(&path).unwrap();
    path
}

/// A zip that is a bundle enough to sign, with a comment of its own.
fn commented(dir: &Path, comment: &str) -> PathBuf {
    let path = dir.join("commented.mcpb");
    let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    zip.start_file("manifest.json", options).unwrap();
    zip.write_all(br#"{"manifest_version": "0.3", "name": "commented"}"#)
        .unwrap();
    zip.set_comment(comment).unwrap();
    zip.finish().unwrap();
    path
}

/// What a bundle says about its signature, which must be one this crate reads.
///
/// The file is read on its own, so tampered bytes are reported as an invalid
/// signature rather than as a bundle that cannot be opened.
fn present(path: &Path) -> Signature {
    match mcpb::signature_at(path, SystemTime::now()).expect("the file reads") {
        SignatureState::Present(signature) => *signature,
        other => panic!("expected a signature block, found {other:?}"),
    }
}

/// The offset of the last `needle` in `bytes`.
fn last(bytes: &[u8], needle: &[u8]) -> usize {
    bytes
        .windows(needle.len())
        .rposition(|window| window == needle)
        .expect("the marker is there")
}

/// The bytes of a bundle with one byte changed.
fn tampered(path: &Path, at: usize) -> Vec<u8> {
    let mut bytes = fs::read(path).unwrap();
    bytes[at] ^= 0xff;
    bytes
}

#[test]
fn a_signed_bundle_verifies_and_its_zip_still_reads() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = bundle_in(dir.path());
    let unsigned = fs::metadata(&path).unwrap().len();

    sign(&path, &signer()).unwrap();

    // The zip is what was there, so an ordinary reader has to accept it.
    let mut archive = zip::ZipArchive::new(fs::File::open(&path).unwrap()).unwrap();
    assert_eq!(archive.len(), 2);
    let mut manifest = String::new();
    std::io::Read::read_to_string(
        &mut archive.by_name("manifest.json").unwrap(),
        &mut manifest,
    )
    .unwrap();
    assert!(manifest.contains("signed-server"), "{manifest}");

    let bundle = Bundle::open(&path).unwrap();
    assert!(bundle.has_signature_block());
    assert_eq!(bundle.entries().len(), 2);
    assert_eq!(bundle.size(), fs::metadata(&path).unwrap().len());
    assert!(bundle.size() > unsigned);

    let signature = present(&path);
    assert_eq!(signature.crypto, CryptoValidity::Valid);
    assert_eq!(signature.layout, Some(Layout::Canonical));
    assert_eq!(signature.validity, CertificateValidity::Valid);
    assert!(signature.signed_at.is_some());

    let signer = signature.signer.clone().expect("a signer certificate");
    assert_eq!(signer.subject, "CN=mcpb test signer");
    assert_eq!(signer.issuer, "CN=mcpb test CA");
    assert!(!signer.self_signed);
    assert_eq!(signer.sha256.len(), 64, "a SHA-256 fingerprint");
}

#[test]
fn unsigning_restores_the_bytes_that_were_signed() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = bundle_in(dir.path());
    let unsigned = fs::read(&path).unwrap();

    sign(&path, &signer()).unwrap();
    assert_ne!(fs::read(&path).unwrap(), unsigned);

    unsign(&path).unwrap();
    assert_eq!(fs::read(&path).unwrap(), unsigned);
    assert!(!Bundle::open(&path).unwrap().has_signature_block());

    let error = unsign(&path).unwrap_err();
    assert!(error.to_string().contains("is not signed"), "{error}");
}

#[test]
fn a_comment_of_the_zip_is_kept_and_restored() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = commented(dir.path(), "a note for whoever reads this");
    let unsigned = fs::read(&path).unwrap();

    sign(&path, &signer()).unwrap();
    assert_eq!(present(&path).crypto, CryptoValidity::Valid);

    unsign(&path).unwrap();
    assert_eq!(fs::read(&path).unwrap(), unsigned, "the comment comes back");
}

#[test]
fn the_signature_covers_every_byte_before_it() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = bundle_in(dir.path());
    sign(&path, &signer()).unwrap();
    let signed = fs::read(&path).unwrap();
    let header = last(&signed, b"MCPB_SIG_V1");
    let end = last(&signed, b"PK\x05\x06");
    let changed = dir.path().join("changed.mcpb");

    // A byte of the zip itself, where the manifest is written.
    fs::write(&changed, tampered(&path, 60)).unwrap();
    let signature = present(&changed);
    assert!(
        matches!(signature.crypto, CryptoValidity::Invalid(_)),
        "{:?}",
        signature.crypto
    );

    for (what, at) in [
        ("the signature's bytes", header + 40),
        ("the signature's length", header + 12),
    ] {
        fs::write(&changed, tampered(&path, at)).unwrap();
        assert_ne!(present(&changed).crypto, CryptoValidity::Valid, "{what}");
    }

    // The comment length the signature covers: the block no longer covers the
    // bytes after it, which is no layout this crate writes, so it is named rather
    // than verified.
    fs::write(&changed, tampered(&path, end + 20)).unwrap();
    assert_eq!(
        mcpb::signature_at(&changed, SystemTime::now()).unwrap(),
        SignatureState::Foreign(ForeignLayout::Unknown)
    );
}

#[test]
fn a_swapped_block_does_not_verify() {
    let dir = tempfile::TempDir::new().unwrap();
    let mine = bundle_in(dir.path());
    sign(&mine, &signer()).unwrap();

    // Another bundle, signed over other bytes.
    let other_dir = dir.path().join("other");
    fs::create_dir_all(&other_dir).unwrap();
    let theirs = bundle_in(&other_dir);
    let mut bytes = fs::read(&theirs).unwrap();
    bytes[20] ^= 0xff;
    fs::write(&theirs, &bytes).unwrap();
    sign(&theirs, &signer()).unwrap();

    // Their block on my bytes, with my comment length raised to declare it.
    let mut spliced = fs::read(&mine).unwrap();
    let header = last(&spliced, b"MCPB_SIG_V1");
    let mine_block = spliced.len() - header;
    let theirs_bytes = fs::read(&theirs).unwrap();
    let block = &theirs_bytes[last(&theirs_bytes, b"MCPB_SIG_V1")..];

    let end = last(&spliced, b"PK\x05\x06");
    let comment = u16::from_le_bytes([spliced[end + 20], spliced[end + 21]]) as usize;
    let original = comment - mine_block;
    spliced[end + 20..end + 22].copy_from_slice(&((original + block.len()) as u16).to_le_bytes());
    spliced.truncate(header);
    spliced.extend_from_slice(block);

    let path = dir.path().join("spliced.mcpb");
    fs::write(&path, &spliced).unwrap();
    let signature = present(&path);
    assert!(
        matches!(signature.crypto, CryptoValidity::Invalid(_)),
        "{:?}",
        signature.crypto
    );
}

#[test]
fn an_appended_byte_makes_the_block_broken() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = bundle_in(dir.path());
    sign(&path, &signer()).unwrap();

    let mut bytes = fs::read(&path).unwrap();
    bytes.push(0);
    let broken = dir.path().join("broken.mcpb");
    fs::write(&broken, &bytes).unwrap();

    let signature = present(&broken);
    assert_eq!(signature.layout, None);
    assert!(
        matches!(&signature.crypto, CryptoValidity::Malformed(error)
            if error.to_string().contains("after its signature block")),
        "{:?}",
        signature.crypto
    );
}

#[test]
fn the_markers_of_a_signed_bundle_inside_one_are_not_a_block() {
    // A stored entry can hold a signed bundle, as a bundle shipping fixtures does.
    // Its footer is inside this bundle, and this bundle's own zip ends after it, so
    // the file is unsigned: not signed-but-broken.
    let dir = tempfile::TempDir::new().unwrap();
    let nested = dir.path().join("nested.mcpb");
    let mut zip = zip::ZipWriter::new(fs::File::create(&nested).unwrap());
    zip.start_file(
        "fixtures/upstream-main.mcpb",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(&fs::read(fixture("upstream-main.mcpb")).unwrap())
        .unwrap();
    zip.start_file("manifest.json", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(br#"{"manifest_version": "0.3", "name": "nested"}"#)
        .unwrap();
    zip.finish().unwrap();

    assert_eq!(
        mcpb::signature_at(&nested, SystemTime::now()).unwrap(),
        SignatureState::None
    );

    // Which means it can be signed, rather than refusing a block it would have to
    // remove first.
    let key = signer();
    sign(&nested, &key).unwrap();
    assert!(matches!(
        mcpb::signature_at(&nested, SystemTime::now()).unwrap(),
        SignatureState::Present(_)
    ));
}

#[test]
fn the_upstream_layouts_are_named_not_verified() {
    for (name, expected) in [
        ("upstream-212.mcpb", ForeignLayout::Mcpb212),
        ("upstream-main.mcpb", ForeignLayout::UpstreamMain),
    ] {
        let bundle = Bundle::open(fixture(name)).unwrap();
        assert_eq!(
            bundle.signature().unwrap(),
            SignatureState::Foreign(expected),
            "{name}"
        );
        assert!(bundle.has_signature_block(), "{name}");
    }
}

#[test]
fn a_chain_with_an_intermediate_reaches_its_anchor() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = bundle_in(dir.path());
    sign(
        &path,
        &identity("chained.pem", "chained.key", &["intermediate.pem"]),
    )
    .unwrap();

    let signature = present(&path);
    assert_eq!(signature.crypto, CryptoValidity::Valid);
    assert_eq!(signature.validity, CertificateValidity::Valid);
    assert_eq!(
        check_trust(&signature, SystemTime::now(), Some(&fixture("ca.pem"))),
        Trust::Trusted
    );
}

#[test]
fn a_chain_link_the_pure_rust_check_cannot_verify_is_untrusted() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = bundle_in(dir.path());
    sign(&path, &identity("ec-signed.pem", "ec-signed.key", &[])).unwrap();

    // The signature itself is fine; only the certificate above it is signed with
    // an algorithm `--ca-file` does not implement.
    let signature = present(&path);
    assert_eq!(signature.crypto, CryptoValidity::Valid);

    match check_trust(&signature, SystemTime::now(), Some(&fixture("ec-ca.pem"))) {
        Trust::Untrusted(TrustProblem::UnsupportedChainAlgorithm {
            certificate,
            algorithm,
        }) => {
            assert_eq!(certificate, "CN=mcpb test EC-signed signer");
            assert!(algorithm.contains("ECDSA"), "{algorithm}");
        }
        other => panic!("expected an unsupported link, found {other:?}"),
    }
}

#[test]
fn a_ca_that_did_not_sign_it_is_untrusted() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = bundle_in(dir.path());
    sign(&path, &signer()).unwrap();

    let signature = present(&path);
    let trust = check_trust(&signature, SystemTime::now(), Some(&fixture("ec-ca.pem")));
    assert!(matches!(trust, Trust::Untrusted(_)), "{trust:?}");
}

#[test]
fn an_expired_certificate_is_reported_and_refused() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = bundle_in(dir.path());

    // The certificate expired in 2020, so it cannot sign a bundle now...
    let error = sign(&path, &identity("expired.pem", "expired.key", &[])).unwrap_err();
    assert!(error.to_string().contains("expired"), "{error}");
    assert!(!Bundle::open(&path).unwrap().has_signature_block());

    // ...but a bundle signed while it was valid reports how its certificate has
    // aged since.
    let old = dir.path().join("old.mcpb");
    fs::copy(&path, &old).unwrap();
    sign_with(
        &old,
        &identity("expired.pem", "expired.key", &[]),
        &SignOptions {
            force: false,
            signing_time: Some(
                SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_500_000_000),
            ),
        },
    )
    .unwrap();

    assert!(Bundle::open(&old).unwrap().has_signature_block());
    let signature = present(&old);
    assert_eq!(signature.crypto, CryptoValidity::Valid);
    assert_eq!(signature.validity, CertificateValidity::Expired);
    let signed_at = signature.signed_at.expect("a claimed signing time");
    assert_eq!(
        signed_at,
        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_500_000_000)
    );
}

#[test]
fn a_self_signed_bundle_says_so() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = bundle_in(dir.path());
    sign(&path, &identity("self-signed.pem", "self-signed.key", &[])).unwrap();

    let signature = present(&path);
    assert_eq!(signature.crypto, CryptoValidity::Valid);
    let signer = signature.signer.clone().expect("a signer certificate");
    assert!(signer.self_signed);
    assert_eq!(signer.subject, signer.issuer);
}

#[test]
fn signing_needs_a_key_that_matches_the_certificate() {
    let error = SigningKey::load(&fixture("leaf.pem"), &fixture("other.key"), &[], || {
        Ok(String::new())
    })
    .unwrap_err();
    assert!(
        error.to_string().contains("not the certificate's"),
        "{error}"
    );
}

#[test]
fn signing_refuses_a_certificate_that_may_not_sign_code() {
    let error = SigningKey::load(&fixture("no-eku.pem"), &fixture("no-eku.key"), &[], || {
        Ok(String::new())
    })
    .unwrap_err();
    assert!(error.to_string().contains("codeSigning"), "{error}");
}

#[test]
fn signing_refuses_a_key_below_2048_bits() {
    let error = SigningKey::load(&fixture("weak.pem"), &fixture("weak.key"), &[], || {
        Ok(String::new())
    })
    .unwrap_err();
    assert!(error.to_string().contains("at least 2048"), "{error}");
}

#[test]
fn signing_refuses_a_comment_that_cannot_hold_the_signature() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = commented(dir.path(), &"c".repeat(65_000));
    let before = fs::read(&path).unwrap();

    let error = sign(&path, &signer()).unwrap_err();
    assert!(error.to_string().contains("cannot hold"), "{error}");
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn double_signing_is_refused_and_force_replaces() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = bundle_in(dir.path());
    sign(&path, &signer()).unwrap();
    let once = fs::read(&path).unwrap();

    let error = sign(&path, &signer()).unwrap_err();
    assert!(error.to_string().contains("--force"), "{error}");
    assert_eq!(fs::read(&path).unwrap(), once);

    sign_with(
        &path,
        &identity("self-signed.pem", "self-signed.key", &[]),
        &SignOptions {
            force: true,
            signing_time: None,
        },
    )
    .unwrap();
    let signature = present(&path);
    assert!(signature.signer.unwrap().self_signed, "the new signer");
}

#[test]
fn a_key_can_be_encrypted_or_in_its_old_shape() {
    let certificate = fixture("leaf.pem");

    let wrong = SigningKey::load(&certificate, &fixture("encrypted.key"), &[], || {
        Ok("not it".to_string())
    })
    .unwrap_err();
    assert!(wrong.to_string().contains("cannot be decrypted"), "{wrong}");

    for (key, passphrase) in [
        ("encrypted.key", "mcpb-test"),
        ("pkcs1.key", ""),
        ("leaf.key", ""),
    ] {
        let identity = SigningKey::load(&certificate, &fixture(key), &[], || {
            Ok(passphrase.to_string())
        })
        .unwrap_or_else(|error| panic!("{key}: {error}"));
        assert_eq!(
            identity
                .certificate()
                .tbs_certificate()
                .subject()
                .to_string(),
            "CN=mcpb test signer"
        );
    }
}

#[test]
fn packing_signs_what_it_writes() {
    let dir = tempfile::TempDir::new().unwrap();
    let binary = dir.path().join("server-binary");
    fs::write(&binary, b"\x7fELF fake").unwrap();
    let spec = BundleSpec {
        name: "packed-signed".to_string(),
        version: "2.0.0".parse().unwrap(),
        description: "A server to sign.".to_string(),
        author: Author::new("Test"),
        executables: BTreeMap::from([(Platform::Linux, Executable::new(&binary, "x64"))]),
        ..BundleSpec::default()
    };

    let path = dir.path().join("packed-signed.mcpb");
    spec.pack_signed(&path, &signer()).unwrap();
    assert_eq!(present(&path).crypto, CryptoValidity::Valid);

    // A failed signing leaves no bundle behind.
    let failed = dir.path().join("failed.mcpb");
    spec.pack_signed(&failed, &identity("expired.pem", "expired.key", &[]))
        .unwrap_err();
    assert!(!failed.exists());
}

#[test]
fn a_prepared_bundle_takes_a_signature_made_elsewhere() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = bundle_in(dir.path());

    prepare_for_signing(&path, 8192).unwrap();
    let prepared = fs::read(&path).unwrap();
    assert!(!Bundle::open(&path).unwrap().has_signature_block());
    assert_eq!(prepared.len(), fs::metadata(&path).unwrap().len() as usize);

    // Prepared once, and never twice: a second slot would move the bytes an
    // external signer already signed.
    let error = prepare_for_signing(&path, 8192).unwrap_err();
    assert!(error.to_string().contains("comment"), "{error}");

    // A signature over other bytes does not fit these: it is refused, and the
    // prepared bundle is left exactly as it was.
    let elsewhere = dir.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let other = bundle_in(&elsewhere);
    let mut bytes = fs::read(&other).unwrap();
    bytes[20] ^= 0xff;
    fs::write(&other, &bytes).unwrap();
    sign(&other, &signer()).unwrap();

    let signed = fs::read(&other).unwrap();
    let header = last(&signed, b"MCPB_SIG_V1");
    let length = u32::from_le_bytes(signed[header + 11..header + 15].try_into().unwrap()) as usize;
    let der = dir.path().join("sig.der");
    fs::write(&der, &signed[header + 15..header + 15 + length]).unwrap();

    let error = apply_signature(&path, &der).unwrap_err();
    assert!(error.to_string().contains("does not verify"), "{error}");
    assert_eq!(fs::read(&path).unwrap(), prepared);
}

#[test]
fn applying_a_signature_needs_a_prepared_bundle() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = bundle_in(dir.path());
    let before = fs::read(&path).unwrap();

    let signature = dir.path().join("sig.der");
    fs::write(&signature, b"not a signature").unwrap();

    let error = apply_signature(&path, &signature).unwrap_err();
    assert!(error.to_string().contains("not prepared"), "{error}");
    assert_eq!(fs::read(&path).unwrap(), before);

    // A slot smaller than the signature cannot hold it.
    prepare_for_signing(&path, 4096).unwrap();
    let prepared = fs::read(&path).unwrap();
    fs::write(&signature, vec![0u8; 5000]).unwrap();
    let error = apply_signature(&path, &signature).unwrap_err();
    assert!(error.to_string().contains("slot"), "{error}");
    assert_eq!(fs::read(&path).unwrap(), prepared);
}

#[test]
fn a_file_that_only_looks_like_a_zip_is_refused() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("fake.mcpb");
    // The bytes of an end of central directory signature with no record behind
    // them: reading one has to fit, not to read past the end of the file.
    let mut bytes = b"not a zip at all".repeat(4);
    bytes.extend_from_slice(b"PK\x05\x06");
    fs::write(&path, &bytes).unwrap();

    assert_eq!(
        mcpb::signature(&path).unwrap(),
        SignatureState::None,
        "no block to speak of"
    );

    let error = sign(&path, &signer()).unwrap_err();
    assert!(error.to_string().contains("zip"), "{error}");
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[test]
fn a_comment_that_runs_past_the_file_is_refused() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = commented(dir.path(), "");
    let mut bytes = fs::read(&path).unwrap();
    let end = last(&bytes, b"PK\x05\x06");
    bytes[end + 20..end + 22].copy_from_slice(&u16::MAX.to_le_bytes());
    fs::write(&path, &bytes).unwrap();

    // Reading it is not a panic, and signing says what is wrong.
    assert_eq!(mcpb::signature(&path).unwrap(), SignatureState::None);
    let error = sign(&path, &signer()).unwrap_err();
    assert!(error.to_string().contains("past the end"), "{error}");
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[test]
fn a_block_the_zip_does_not_declare_is_not_removed() {
    // What a 2.1.2 bundle carries was appended after its comment, so cutting it
    // off would rewrite bytes that are not this crate's to guess at.
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("upstream.mcpb");
    fs::copy(fixture("upstream-212.mcpb"), &path).unwrap();
    let before = fs::read(&path).unwrap();

    let error = sign_with(
        &path,
        &signer(),
        &SignOptions {
            force: true,
            signing_time: None,
        },
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("not written by this crate"),
        "{error}"
    );
    assert_eq!(fs::read(&path).unwrap(), before);

    let error = unsign(&path).unwrap_err();
    assert!(
        error.to_string().contains("not written by this crate"),
        "{error}"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn force_replaces_a_block_the_zip_declares() {
    // Upstream `main` declares its block in the comment, so removing it restores
    // the zip exactly, and `--force` can sign the result properly.
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("upstream.mcpb");
    fs::copy(fixture("upstream-main.mcpb"), &path).unwrap();
    let foreign = fs::read(&path).unwrap();

    sign_with(
        &path,
        &signer(),
        &SignOptions {
            force: true,
            signing_time: None,
        },
    )
    .unwrap();
    assert_eq!(present(&path).crypto, CryptoValidity::Valid);

    // Unsigning gives back the bytes upstream signed: the zip as it was, with the
    // comment length up to the block no longer declared.
    unsign(&path).unwrap();
    let recovered = fs::read(&path).unwrap();
    let end = last(&recovered, b"PK\x05\x06");
    assert!(recovered.len() < foreign.len());
    assert_eq!(&recovered[..end + 20], &foreign[..end + 20]);
    assert_eq!(&recovered[end + 20..end + 22], &[0, 0]);
    assert!(!Bundle::open(&path).unwrap().has_signature_block());
}
