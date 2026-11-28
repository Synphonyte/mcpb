//! Whether the certificate that signed a bundle is trusted.
//!
//! The library verifies cryptography and never runs another program; this module
//! is where a platform's own policy is asked, and `--ca-file` is the portable
//! answer for a private certificate authority. Linux has no code-signing policy
//! store, so plain `mcpb verify` does not claim to establish a publisher there.

use std::path::Path;
use std::time::SystemTime;

use crate::signature::{Signature, Trust};

/// Whether the chain that signed `signature` reaches a trusted anchor.
///
/// `ca_file` is the `--ca-file` a caller supplied. Without one, macOS and Windows
/// use their own chain builders and every other platform reports that trust was
/// not evaluated at all.
pub fn evaluate(signature: &Signature, at: SystemTime, ca_file: Option<&Path>) -> Trust {
    let certificates = &signature.certificates;
    if certificates.is_empty() {
        return Trust::NotEvaluated;
    }

    #[cfg(feature = "trust")]
    match ca_file {
        Some(file) => anchored(certificates, at, file),
        None => platform(certificates, at),
    }

    #[cfg(not(feature = "trust"))]
    {
        let _ = (at, ca_file);
        Trust::NotEvaluated
    }
}

/// macOS and Windows ask their own chain builder; everywhere else has none to ask.
#[cfg(all(feature = "trust", any(target_os = "macos", target_os = "windows")))]
fn platform(certificates: &[x509_cert::Certificate], at: SystemTime) -> Trust {
    #[cfg(target_os = "macos")]
    let format = system::Format::Pem;
    #[cfg(target_os = "windows")]
    let format = system::Format::Der;

    let files = match system::Certificates::write(certificates, format) {
        Ok(files) => files,
        Err(error) => return Trust::Error(error),
    };

    #[cfg(target_os = "macos")]
    let command = match system::macos(&files, certificates.len(), at) {
        Ok(command) => command,
        Err(error) => return Trust::Error(error),
    };
    #[cfg(target_os = "windows")]
    let command = match system::windows(&files, certificates.len(), at) {
        Ok(command) => command,
        Err(error) => return Trust::Error(error),
    };

    system::run(command, system::PROGRAM)
}

/// Linux and the rest: no code-signing policy store to ask.
#[cfg(all(
    feature = "trust",
    not(any(target_os = "macos", target_os = "windows"))
))]
fn platform(_certificates: &[x509_cert::Certificate], _at: SystemTime) -> Trust {
    Trust::NotEvaluated
}

/// Running the platform's chain builder over certificates written to disk.
#[cfg(all(feature = "trust", any(target_os = "macos", target_os = "windows")))]
mod system {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::SystemTime;

    use x509_cert::Certificate;
    #[cfg(target_os = "windows")]
    use x509_cert::der::Encode;
    #[cfg(target_os = "macos")]
    use x509_cert::der::{DateTime, EncodePem, pem::LineEnding};

    use crate::signature::{Trust, TrustError, TrustProblem};

    #[cfg(target_os = "macos")]
    pub(super) const PROGRAM: &str = "security verify-cert";
    #[cfg(target_os = "windows")]
    pub(super) const PROGRAM: &str = "powershell X509Chain";

    /// The exit code that means "the chain was built and is not trusted".
    ///
    /// Every other failure is the check itself failing, which is a trust error
    /// rather than a verdict: `security` has one code for both, so only the
    /// PowerShell script, which picks its own, can tell them apart.
    #[cfg(target_os = "macos")]
    pub(super) const NOT_TRUSTED: i32 = 1;
    #[cfg(target_os = "windows")]
    pub(super) const NOT_TRUSTED: i32 = 2;

    /// How a verifier reads a certificate it is handed.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(super) enum Format {
        /// `security verify-cert -c` reads PEM.
        #[cfg(target_os = "macos")]
        Pem,
        /// `X509Certificate2`'s file constructor reads DER.
        #[cfg(target_os = "windows")]
        Der,
    }

    impl Format {
        fn extension(self) -> &'static str {
            match self {
                #[cfg(target_os = "macos")]
                Self::Pem => "pem",
                #[cfg(target_os = "windows")]
                Self::Der => "cer",
            }
        }
    }

    /// A temporary directory of certificates, removed when it goes out of scope.
    pub(super) struct Certificates(PathBuf, Format);

    impl Certificates {
        /// Writes `certificates` as numbered files in `format`, leaf first.
        pub(super) fn write(
            certificates: &[Certificate],
            format: Format,
        ) -> Result<Self, TrustError> {
            use std::sync::atomic::{AtomicUsize, Ordering};

            static COUNT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "mcpb-trust-{}-{}",
                std::process::id(),
                COUNT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).map_err(|error| TrustError::ToolFailed {
                program: "the temporary directory".to_string(),
                message: format!("can't create {}: {error}", dir.display()),
            })?;

            let files = Self(dir.clone(), format);
            for (index, certificate) in certificates.iter().enumerate() {
                let bytes = match format {
                    #[cfg(target_os = "macos")]
                    Format::Pem => certificate
                        .to_pem(LineEnding::LF)
                        .map(String::into_bytes)
                        .map_err(|error| {
                            TrustError::Configuration(format!(
                                "a certificate cannot be written as PEM: {error}"
                            ))
                        })?,
                    #[cfg(target_os = "windows")]
                    Format::Der => certificate.to_der().map_err(|error| {
                        TrustError::Configuration(format!(
                            "a certificate cannot be written as DER: {error}"
                        ))
                    })?,
                };

                let path = files.at(index);
                fs::write(&path, bytes).map_err(|error| TrustError::ToolFailed {
                    program: "the temporary directory".to_string(),
                    message: format!("can't write {}: {error}", path.display()),
                })?;
            }

            Ok(files)
        }

        /// The file of the certificate at `index`.
        pub(super) fn at(&self, index: usize) -> PathBuf {
            self.0.join(format!("{index}.{}", self.1.extension()))
        }

        /// A file in the same directory, such as the Windows check's script.
        #[cfg(target_os = "windows")]
        fn beside(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Certificates {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// `security verify-cert` with the code-signing policy, at `at`.
    #[cfg(target_os = "macos")]
    pub(super) fn macos(
        files: &Certificates,
        count: usize,
        at: SystemTime,
    ) -> Result<Command, TrustError> {
        let mut command = Command::new("security");
        command.arg("verify-cert");
        // One `-c` per certificate, the signer first: the rest are what the
        // chain may be built with.
        for index in 0..count {
            command.arg("-c").arg(files.at(index));
        }
        command.arg("-p").arg("codeSign");
        // The chain is judged at the time the caller asked about, which is what
        // `--at` means; without this the tool would use the clock instead, and a
        // time it cannot name is refused rather than checked as now.
        command.arg("-d").arg(date(at)?);
        Ok(command)
    }

    /// `security`'s `-d` date: `YYYY-MM-DD-hh:mm:ss`, in GMT.
    #[cfg(target_os = "macos")]
    fn date(at: SystemTime) -> Result<String, TrustError> {
        let time = DateTime::from_system_time(at).map_err(|error| {
            TrustError::Configuration(format!("the time to check at cannot be written: {error}"))
        })?;
        Ok(format!(
            "{:04}-{:02}-{:02}-{:02}:{:02}:{:02}",
            time.year(),
            time.month(),
            time.day(),
            time.hour(),
            time.minutes(),
            time.seconds()
        ))
    }

    /// `X509Chain` with the code-signing application policy, at `at`.
    #[cfg(target_os = "windows")]
    pub(super) fn windows(
        files: &Certificates,
        count: usize,
        at: SystemTime,
    ) -> Result<Command, TrustError> {
        // A script file with the certificates as arguments: `-File` is the one
        // form that hands a multi-line script and its paths over without either
        // being quoted into the other. The first argument is the time to judge
        // the chain at, in seconds since the Unix epoch.
        let script = r#"$ErrorActionPreference = 'Stop'
$seconds = [long]$args[0]
$paths = @($args[1..($args.Length - 1)])
$leaf = [System.Security.Cryptography.X509Certificates.X509Certificate2]::new($paths[0])
$chain = [System.Security.Cryptography.X509Certificates.X509Chain]::new()
$chain.ChainPolicy.RevocationMode = 'Online'
$chain.ChainPolicy.ApplicationPolicy.Add([System.Security.Cryptography.Oid]::new('1.3.6.1.5.5.7.3.3'))
$chain.ChainPolicy.VerificationTime = [System.DateTimeOffset]::FromUnixTimeSeconds($seconds).LocalDateTime
for ($i = 1; $i -lt $paths.Length; $i++) {
  $chain.ChainPolicy.ExtraStore.Add([System.Security.Cryptography.X509Certificates.X509Certificate2]::new($paths[$i]))
}
if (-not $chain.Build($leaf)) {
  [Console]::Error.WriteLine(($chain.ChainStatus | ForEach-Object { $_.StatusInformation.Trim() } | Out-String))
  exit 2
}
"#;

        let path = files.beside("check.ps1");
        fs::write(&path, script).map_err(|error| TrustError::ToolFailed {
            program: PROGRAM.to_string(),
            message: format!("can't write {}: {error}", path.display()),
        })?;

        let mut command = Command::new("powershell");
        command
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(&path)
            .arg(seconds(at).to_string());
        for index in 0..count {
            command.arg(files.at(index));
        }
        Ok(command)
    }

    /// `at` in seconds since the Unix epoch, which is what `DateTimeOffset` reads.
    #[cfg(target_os = "windows")]
    fn seconds(at: SystemTime) -> u64 {
        at.duration_since(SystemTime::UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or_default()
    }

    /// Runs a chain check and reads its verdict.
    pub(super) fn run(mut command: Command, program: &str) -> Trust {
        match command.output() {
            Ok(output) if output.status.success() => Trust::Trusted,
            Ok(output) if output.status.code() == Some(NOT_TRUSTED) => Trust::Untrusted(
                TrustProblem::ChainInvalid(reason(&output.stderr, &output.stdout)),
            ),
            Ok(output) => Trust::Error(TrustError::ToolFailed {
                program: program.to_string(),
                message: match output.status.code() {
                    Some(code) => format!(
                        "it exited with {code}: {}",
                        reason(&output.stderr, &output.stdout)
                    ),
                    None => "it was killed before it answered".to_string(),
                },
            }),
            Err(error) => Trust::Error(TrustError::ToolFailed {
                program: program.to_string(),
                message: error.to_string(),
            }),
        }
    }

    /// The first line a tool said, preferring what it wrote to standard error.
    fn reason(stderr: &[u8], stdout: &[u8]) -> String {
        String::from_utf8_lossy(stderr)
            .lines()
            .chain(String::from_utf8_lossy(stdout).lines())
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("the chain does not lead to a trusted certificate")
            .to_string()
    }
}

/// `--ca-file`: a pure-Rust chain built from the anchors the caller named.
#[cfg(feature = "trust")]
fn anchored(certificates: &[x509_cert::Certificate], at: SystemTime, file: &Path) -> Trust {
    use rustls_pki_types::{CertificateDer, UnixTime};
    use webpki::{EndEntityCert, KeyUsage, anchor_from_trusted_cert};

    use crate::signature::{Trust, TrustError, TrustProblem};

    let roots = match crate::signing::read_certificates(file) {
        Ok(roots) => roots,
        Err(error) => return Trust::Error(TrustError::Configuration(error.to_string())),
    };

    let mut roots_der = Vec::with_capacity(roots.len());
    for root in &roots {
        match der(root) {
            Ok(der) => roots_der.push(CertificateDer::from(der)),
            Err(error) => return Trust::Error(error),
        }
    }
    let trust_anchors: Vec<_> = match roots_der
        .iter()
        .map(|der| anchor_from_trusted_cert(der))
        .collect::<Result<_, _>>()
    {
        Ok(anchors) => anchors,
        Err(error) => {
            return Trust::Error(TrustError::Configuration(format!(
                "{} holds a certificate that is no trust anchor: {error}",
                file.display()
            )));
        }
    };

    let mut intermediates = Vec::new();
    for intermediate in &certificates[1..] {
        match der(intermediate) {
            Ok(der) => intermediates.push(CertificateDer::from(der)),
            Err(error) => return Trust::Error(error),
        }
    }

    let leaf = match der(&certificates[0]).map(CertificateDer::from) {
        Ok(leaf) => leaf,
        Err(error) => return Trust::Error(error),
    };
    let end_entity = match EndEntityCert::try_from(&leaf) {
        Ok(end_entity) => end_entity,
        Err(error) => {
            return Trust::Untrusted(TrustProblem::ChainInvalid(format!(
                "the signer certificate cannot be read: {error}"
            )));
        }
    };

    let time = UnixTime::since_unix_epoch(
        at.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default(),
    );

    match end_entity.verify_for_usage(
        &[&RSA_PKCS1_SHA256],
        &trust_anchors,
        &intermediates,
        time,
        // webpki applies the policy to every certificate in the path, and an
        // intermediate with a TLS-only EKU must not sign code. The signer's own
        // codeSigning EKU is required separately, when its signature is verified,
        // because webpki would accept a signer that declares none at all.
        KeyUsage::required_if_present(CODE_SIGNING),
        None,
        None,
    ) {
        Ok(_) => Trust::Trusted,
        // The path is built before its links are verified, so an algorithm this
        // provider cannot check is only reported when the path that was built has
        // one: a certificate in the set that is not part of the path says nothing
        // about this signature. A link it cannot check makes the chain untrusted
        // with a reason, which is a trust answer: the bundle's own signature is
        // not in question.
        Err(webpki::Error::UnsupportedSignatureAlgorithmContext(_)) => {
            Trust::Untrusted(unsupported_link(certificates, &roots).unwrap_or_else(|| {
                TrustProblem::UnsupportedChainAlgorithm {
                    certificate: "a certificate in the chain".to_string(),
                    algorithm: "an algorithm other than RSA PKCS#1 v1.5 with SHA-256".to_string(),
                }
            }))
        }
        Err(error) => Trust::Untrusted(TrustProblem::ChainInvalid(error.to_string())),
    }
}

/// The `codeSigning` extended key usage, as the OID's value bytes.
#[cfg(feature = "trust")]
const CODE_SIGNING: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x03];

/// RSA PKCS#1 v1.5 with SHA-256, verified with the `rsa` crate, so that trusting a
/// chain links no C and no assembly.
#[cfg(feature = "trust")]
#[derive(Debug)]
struct RsaPkcs1Sha256;

#[cfg(feature = "trust")]
static RSA_PKCS1_SHA256: RsaPkcs1Sha256 = RsaPkcs1Sha256;

#[cfg(feature = "trust")]
impl rustls_pki_types::SignatureVerificationAlgorithm for RsaPkcs1Sha256 {
    fn verify_signature(
        &self,
        public_key: &[u8],
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), rustls_pki_types::InvalidSignature> {
        use rsa::pkcs1::DecodeRsaPublicKey;
        use rsa::pkcs1v15::{Signature, VerifyingKey};
        use rsa::signature::Verifier;
        use rsa::traits::PublicKeyParts;
        use sha2::Sha256;

        // webpki hands over the `subjectPublicKey`'s bytes, which for an RSA key
        // is a PKCS#1 `RSAPublicKey`.
        let key = rsa::PublicKey::from_pkcs1_der(public_key)
            .map_err(|_| rustls_pki_types::InvalidSignature)?;
        // The same floor the signer's key has to clear: a chain whose links are
        // signed by a weaker key is not one this crate checks.
        if key.size() * 8 < crate::signing::MIN_KEY_BITS {
            return Err(rustls_pki_types::InvalidSignature);
        }
        let signature =
            Signature::try_from(signature).map_err(|_| rustls_pki_types::InvalidSignature)?;

        VerifyingKey::<Sha256>::new(key)
            .verify(message, &signature)
            .map_err(|_| rustls_pki_types::InvalidSignature)
    }

    fn public_key_alg_id(&self) -> rustls_pki_types::AlgorithmIdentifier {
        rustls_pki_types::alg_id::RSA_ENCRYPTION
    }

    fn signature_alg_id(&self) -> rustls_pki_types::AlgorithmIdentifier {
        rustls_pki_types::alg_id::RSA_PKCS1_SHA256
    }
}

/// The first certificate in the chain whose signature the pure-Rust check does not
/// implement, if any.
#[cfg(feature = "trust")]
fn unsupported_link(
    certificates: &[x509_cert::Certificate],
    roots: &[x509_cert::Certificate],
) -> Option<crate::signature::TrustProblem> {
    use const_oid::db::rfc5912::SHA_256_WITH_RSA_ENCRYPTION;
    use x509_cert::der::Encode;

    for certificate in certificates {
        let der = certificate.to_der().ok()?;
        // An anchor's own signature is never verified, so its algorithm is
        // irrelevant even when the same certificate is also in the chain.
        let anchored = roots
            .iter()
            .any(|root| root.to_der().is_ok_and(|other| other == der));
        if anchored {
            continue;
        }

        let algorithm = certificate.signature_algorithm().oid;
        if algorithm == SHA_256_WITH_RSA_ENCRYPTION {
            continue;
        }
        return Some(crate::signature::TrustProblem::UnsupportedChainAlgorithm {
            certificate: certificate.tbs_certificate().subject().to_string(),
            algorithm: named(&algorithm),
        });
    }

    None
}

/// The common name of a signature algorithm, or its OID.
#[cfg(feature = "trust")]
fn named(oid: &const_oid::ObjectIdentifier) -> String {
    match oid.to_string().as_str() {
        "1.2.840.113549.1.1.5" => "RSA PKCS#1 v1.5 with SHA-1".to_string(),
        "1.2.840.113549.1.1.10" => "RSA-PSS".to_string(),
        "1.2.840.113549.1.1.12" => "RSA PKCS#1 v1.5 with SHA-384".to_string(),
        "1.2.840.113549.1.1.13" => "RSA PKCS#1 v1.5 with SHA-512".to_string(),
        "1.2.840.10045.4.1" => "ECDSA with SHA-1".to_string(),
        "1.2.840.10045.4.3.2" => "ECDSA with SHA-256".to_string(),
        "1.2.840.10045.4.3.3" => "ECDSA with SHA-384".to_string(),
        "1.2.840.10045.4.3.4" => "ECDSA with SHA-512".to_string(),
        "1.3.101.112" => "Ed25519".to_string(),
        "1.3.101.113" => "Ed448".to_string(),
        other => other.to_string(),
    }
}

/// The DER of a certificate.
#[cfg(feature = "trust")]
fn der(certificate: &x509_cert::Certificate) -> Result<Vec<u8>, crate::signature::TrustError> {
    use x509_cert::der::Encode;

    certificate.to_der().map_err(|error| {
        crate::signature::TrustError::Configuration(format!(
            "a certificate cannot be re-encoded: {error}"
        ))
    })
}
