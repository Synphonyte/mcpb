//! Signing a bundle, preparing one for an external signer, and applying the
//! signature it produced.
//!
//! A signature covers the bytes a client is handed, so it is appended after the
//! zip with the zip's comment length already patched to declare it (see
//! [`crate::signature`]). An external signer (an HSM) cannot be asked twice, so
//! `prepare_for_signing` reserves a zero-padded slot up front and `apply_signature`
//! fills it in: the bytes the HSM signs are the bytes that ship.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use pkcs8::{DecodePrivateKey, EncryptedPrivateKeyInfoRef, SecretDocument};
use rsa::PrivateKey;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use x509_cert::Certificate;
use x509_cert::der::Decode;

use crate::signature::{
    self, COMMENT_MAX, CryptoValidity, FOOTER, HEADER, LENGTH, SignatureError, SignatureState,
};
use crate::{BundleSpec, Error};

/// The smallest reserved slot: room for a signature and the certificate chain.
pub const MIN_SLOT: usize = 4096;

/// The largest reserved slot: the zip's comment length is two bytes.
pub const MAX_SLOT: usize = COMMENT_MAX;

/// The smallest RSA key this crate signs with.
const MIN_KEY_BITS: usize = 2048;

/// How a bundle is signed.
#[derive(Clone, Debug, Default)]
pub struct SignOptions {
    /// Replace a signature block this crate wrote, rather than refusing to sign.
    pub force: bool,
    /// The signing time to claim; the current time when `None`.
    ///
    /// A fixed time is what makes signing the same input produce the same bytes.
    pub signing_time: Option<SystemTime>,
}

/// The identity a bundle is signed with: a certificate, its key and its chain.
///
/// Loading checks that the three go together and that the certificate may sign
/// software, so a bundle is never signed with a key that clients would reject.
pub struct SigningKey {
    certificate: Certificate,
    chain: Vec<Certificate>,
    key: PrivateKey,
}

/// Names the certificate without ever showing the key.
impl std::fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SigningKey")
            .field(
                "certificate",
                &self.certificate.tbs_certificate().subject().to_string(),
            )
            .field("chain", &self.chain.len())
            .finish_non_exhaustive()
    }
}

impl SigningKey {
    /// Reads `certificate`, the private key in `key` and the certificates in
    /// `chain`.
    ///
    /// The key may be PKCS#8, encrypted PKCS#8 or PKCS#1 RSA, in PEM or DER.
    /// `passphrase` is called only when the key turns out to be encrypted, so a
    /// caller can prompt on a terminal only when there is something to ask for.
    pub fn load(
        certificate: &Path,
        key: &Path,
        chain_paths: &[PathBuf],
        passphrase: impl FnOnce() -> Result<String, Error>,
    ) -> Result<Self, Error> {
        let mut certificates = read_certificates(certificate)?;
        if certificates.len() != 1 {
            return Err(Error::Signing(format!(
                "{} holds {} certificates; --cert wants the signer's one",
                certificate.display(),
                certificates.len()
            )));
        }
        let certificate = certificates.remove(0);

        let mut chain: Vec<Certificate> = Vec::new();
        for path in chain_paths {
            chain.extend(read_certificates(path)?);
        }
        // A signature names its signer by issuer and serial, so carrying the
        // signer's own certificate twice would make that name ambiguous.
        chain.retain(|intermediate| *intermediate != certificate);

        let key = read_key(key, passphrase)?;
        Self::check(certificate, chain, key)
    }

    /// Refuses a certificate and key that clients would not accept.
    fn check(
        certificate: Certificate,
        chain: Vec<Certificate>,
        key: PrivateKey,
    ) -> Result<Self, Error> {
        if key.size() * 8 < MIN_KEY_BITS {
            return Err(Error::Signing(format!(
                "its RSA key is {} bits; code signing needs at least {MIN_KEY_BITS}",
                key.size() * 8
            )));
        }

        let theirs = crate::cms::rsa_key(&certificate)
            .map_err(|error| Error::Signing(format!("the certificate cannot sign: {error}")))?;
        if theirs != key.to_public_key() {
            return Err(Error::Signing(
                "the private key is not the certificate's".to_string(),
            ));
        }

        crate::cms::check_code_signing(&certificate).map_err(|error| {
            Error::Signing(format!("the certificate cannot sign code: {error}"))
        })?;

        Ok(Self {
            certificate,
            chain,
            key,
        })
    }

    /// The signer certificate.
    pub fn certificate(&self) -> &Certificate {
        &self.certificate
    }

    /// The certificates supplied after the signer's.
    pub fn chain(&self) -> &[Certificate] {
        &self.chain
    }
}

/// Signs the bundle at `path` in place.
///
/// Refuses a bundle that already carries a signature; use [`SignOptions::force`]
/// to replace one this crate wrote. A failure leaves the file untouched.
pub fn sign(path: &Path, key: &SigningKey) -> Result<(), Error> {
    sign_with(path, key, &SignOptions::default())
}

/// Signs the bundle at `path` in place, as `options` say.
pub fn sign_with(path: &Path, key: &SigningKey, options: &SignOptions) -> Result<(), Error> {
    let bytes = fs::read(path).map_err(|error| Error::read(path, error))?;
    let signed = signed_bytes(&bytes, key, options)
        .map_err(|error| Error::Signing(format!("cannot sign {}: {error}", path.display())))?;

    signature::replace(path, &signed)
}

/// `bytes` with a signature block appended.
fn signed_bytes(
    bytes: &[u8],
    key: &SigningKey,
    options: &SignOptions,
) -> Result<Vec<u8>, SignatureError> {
    let at = options.signing_time.unwrap_or_else(SystemTime::now);
    if at > SystemTime::now() {
        return Err(SignatureError::new(
            "the signing time to claim is in the future",
        ));
    }

    // Whatever is already there decides whether this is a bundle to sign:
    // a block this crate wrote can be replaced, anything else is refused.
    let content = match signature::framing(bytes) {
        signature::Framing::None => bytes.to_vec(),
        signature::Framing::Block(_) if options.force => signature::without_block(bytes)?
            .ok_or_else(|| SignatureError::new("it is not a zip"))?,
        signature::Framing::Block(_) => {
            return Err(SignatureError::new(
                "it already has a signature block; use --force to replace it",
            ));
        }
        signature::Framing::Broken(error) => {
            return Err(SignatureError::new(format!(
                "its signature block cannot be removed: {error}"
            )));
        }
    };

    let end = end_of_zip(&content)?;
    ends_the_file(&content, &end)?;

    let validity = crate::cms::validity(&key.certificate, at);
    if validity != crate::signature::CertificateValidity::Valid {
        return Err(SignatureError::new(format!(
            "the certificate is {validity} at the signing time"
        )));
    }

    // The block's length is part of the comment length, which is part of what is
    // signed, and the signature's own length decides the block's length. RSA
    // signatures have a fixed length, so signing twice settles it; the bound is
    // there so a variable-length signature cannot loop.
    let mut patched = content;
    let mut der = crate::cms::sign(&patched, &key.certificate, &key.chain, &key.key, Some(at))
        .map_err(|error| SignatureError::new(error.to_string()))?;
    let mut stable = false;
    for _ in 0..4 {
        let block = HEADER.len() + LENGTH + der.len() + FOOTER.len();
        let comment = end
            .comment
            .checked_add(block)
            .and_then(|c| u16::try_from(c).ok());
        let Some(comment) = comment else {
            return Err(SignatureError::new(
                "its zip comment cannot hold the signature: the block would be longer than 65535 bytes",
            ));
        };
        signature::set_comment(&mut patched, &end, comment);

        let again = crate::cms::sign(&patched, &key.certificate, &key.chain, &key.key, Some(at))
            .map_err(|error| SignatureError::new(error.to_string()))?;
        stable = again.len() == der.len();
        der = again;
        if stable {
            break;
        }
    }
    if !stable {
        return Err(SignatureError::new(
            "the signature's length does not settle, so the zip comment cannot declare it",
        ));
    }

    let mut signed = patched;
    signed.extend_from_slice(HEADER);
    signed.extend_from_slice(
        &u32::try_from(der.len())
            .expect("a DER signature fits in a u32")
            .to_le_bytes(),
    );
    signed.extend_from_slice(&der);
    signed.extend_from_slice(FOOTER);

    // A bundle this crate cannot verify is one it must not write.
    match signature::state(&signed, at) {
        SignatureState::Present(signature) if signature.crypto == CryptoValidity::Valid => {
            Ok(signed)
        }
        other => Err(SignatureError::new(format!(
            "the signature does not verify once written: {other:?}"
        ))),
    }
}

/// Reserves a zero-filled slot after the zip for an external signer's signature.
///
/// The bundle's bytes never change once this has run, so the bytes an HSM signs
/// are the bytes that ship.
pub fn prepare_for_signing(path: &Path, slot: usize) -> Result<(), Error> {
    let bytes = fs::read(path).map_err(|error| Error::read(path, error))?;
    let prepared = prepared_bytes(&bytes, slot)
        .map_err(|error| Error::Signing(format!("cannot prepare {}: {error}", path.display())))?;

    signature::replace(path, &prepared)
}

/// `bytes` with a reserved slot appended.
fn prepared_bytes(bytes: &[u8], slot: usize) -> Result<Vec<u8>, SignatureError> {
    if !(MIN_SLOT..=MAX_SLOT).contains(&slot) {
        return Err(SignatureError::new(format!(
            "a slot holds between {MIN_SLOT} and {MAX_SLOT} bytes, not {slot}"
        )));
    }

    let end = end_of_zip(bytes)?;
    ends_the_file(bytes, &end)?;
    if end.comment != 0 {
        return Err(SignatureError::new(format!(
            "its zip comment is {} bytes, and a reserved slot has to be the whole \
             comment: sign a rollback copy without one, or prepare before adding it",
            end.comment
        )));
    }

    let comment = u16::try_from(slot)
        .map_err(|_| SignatureError::new("its zip comment cannot hold a slot that long"))?;

    let mut prepared = bytes.to_vec();
    signature::set_comment(&mut prepared, &end, comment);
    prepared.resize(prepared.len() + slot, 0);
    Ok(prepared)
}

/// Fills the reserved slot of a prepared bundle with an external signature.
///
/// `signature` is the detached DER CMS signature over the whole prepared file.
pub fn apply_signature(path: &Path, signature: &Path) -> Result<(), Error> {
    let bytes = fs::read(path).map_err(|error| Error::read(path, error))?;
    let der = fs::read(signature).map_err(|error| Error::read(signature, error))?;

    let signed = applied_bytes(&bytes, &der).map_err(|error| {
        Error::Signing(format!(
            "cannot apply {} to {}: {error}",
            signature.display(),
            path.display()
        ))
    })?;

    signature::replace(path, &signed)
}

/// `bytes` with `der` written into the reserved slot.
fn applied_bytes(bytes: &[u8], der: &[u8]) -> Result<Vec<u8>, SignatureError> {
    let end = end_of_zip(bytes)?;
    ends_the_file(bytes, &end)?;

    let start = bytes.len() - end.comment;
    if end.comment < MIN_SLOT || bytes[start..].iter().any(|byte| *byte != 0) {
        return Err(SignatureError::new(
            "it is not prepared for an external signature: its zip comment is not a zero-filled slot",
        ));
    }

    let block = HEADER.len() + LENGTH + der.len() + FOOTER.len();
    if block > end.comment {
        return Err(SignatureError::new(format!(
            "the signature needs {block} bytes, and its slot holds {}",
            end.comment
        )));
    }

    let mut signed = bytes[..start].to_vec();
    signed.extend_from_slice(HEADER);
    signed.extend_from_slice(
        &u32::try_from(der.len())
            .expect("a DER signature fits in a u32")
            .to_le_bytes(),
    );
    signed.extend_from_slice(der);
    signed.resize(start + end.comment - FOOTER.len(), 0);
    signed.extend_from_slice(FOOTER);

    // The signature an external signer made is checked before it is kept, so a
    // mismatch is an error here rather than a bundle that fails later.
    match signature::state(&signed, SystemTime::now()) {
        SignatureState::Present(signature) if signature.crypto == CryptoValidity::Valid => {
            Ok(signed)
        }
        other => Err(SignatureError::new(format!(
            "the signature does not verify over these bytes: {other:?}"
        ))),
    }
}

impl BundleSpec {
    /// Packs the spec, signs it and writes it, leaving nothing behind on failure.
    pub fn pack_signed(&self, path: &Path, key: &SigningKey) -> Result<(), Error> {
        self.pack_signed_with(path, key, &SignOptions::default())
    }

    /// The same, as `options` say.
    pub fn pack_signed_with(
        &self,
        path: &Path,
        key: &SigningKey,
        options: &SignOptions,
    ) -> Result<(), Error> {
        let temp = path.with_extension(format!("{}.signed.tmp", std::process::id()));
        let signed = self
            .pack(&temp)
            .and_then(|()| sign_with(&temp, key, options));
        let result = signed.and_then(|()| fs::rename(&temp, path).map_err(Error::from));
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }
}

/// The certificates in `path`, as PEM or as one DER certificate.
pub(crate) fn read_certificates(path: &Path) -> Result<Vec<Certificate>, Error> {
    let bytes = fs::read(path).map_err(|error| Error::read(path, error))?;
    let certificates = match std::str::from_utf8(&bytes) {
        Ok(text) if text.contains("-----BEGIN CERTIFICATE-----") => {
            Certificate::load_pem_chain(text.as_bytes())
                .map_err(|error| unreadable(path, &format!("not PEM certificates: {error}")))?
        }
        _ => vec![
            Certificate::from_der(&bytes)
                .map_err(|error| unreadable(path, &format!("not a DER certificate: {error}")))?,
        ],
    };

    if certificates.is_empty() {
        return Err(unreadable(path, "it holds no certificate"));
    }
    Ok(certificates)
}

/// The RSA private key in `path`, as PEM or DER.
fn read_key(
    path: &Path,
    passphrase: impl FnOnce() -> Result<String, Error>,
) -> Result<PrivateKey, Error> {
    let bytes = fs::read(path).map_err(|error| Error::read(path, error))?;

    let (label, der) = match std::str::from_utf8(&bytes) {
        Ok(text) if text.contains("-----BEGIN") => {
            let (label, document) = SecretDocument::from_pem(text)
                .map_err(|error| unreadable(path, &format!("not a PEM document: {error}")))?;
            (Some(label.to_string()), document.to_bytes())
        }
        _ => (None, zeroize::Zeroizing::new(bytes)),
    };

    let key = match label.as_deref() {
        Some("RSA PRIVATE KEY") => PrivateKey::from_pkcs1_der(&der)
            .map_err(|error| unreadable(path, &format!("not an RSA key: {error}")))?,
        Some("PRIVATE KEY") => PrivateKey::from_pkcs8_der(&der)
            .map_err(|error| unreadable(path, &format!("not an RSA key: {error}")))?,
        Some("ENCRYPTED PRIVATE KEY") => decrypt(path, &der, passphrase)?,
        Some(label) => {
            return Err(unreadable(path, &format!("a {label} is not a private key")));
        }
        // DER carries no label, so what the bytes are decides: an encrypted
        // PKCS#8 key, then PKCS#8 and PKCS#1, which differ only in their wrapper.
        None => match EncryptedPrivateKeyInfoRef::try_from(der.as_slice()) {
            Ok(_) => decrypt(path, &der, passphrase)?,
            Err(_) => PrivateKey::from_pkcs8_der(&der)
                .or_else(|_| PrivateKey::from_pkcs1_der(&der))
                .map_err(|error| unreadable(path, &format!("not an RSA private key: {error}")))?,
        },
    };

    Ok(key)
}

/// The key of an encrypted PKCS#8 document, with the passphrase a caller gives.
fn decrypt(
    path: &Path,
    der: &[u8],
    passphrase: impl FnOnce() -> Result<String, Error>,
) -> Result<PrivateKey, Error> {
    let info = EncryptedPrivateKeyInfoRef::try_from(der)
        .map_err(|error| unreadable(path, &format!("not an encrypted key: {error}")))?;
    let password = passphrase()?;
    let document = info
        .decrypt(password.trim())
        .map_err(|error| unreadable(path, &format!("it cannot be decrypted: {error}")))?;

    PrivateKey::from_pkcs8_der(document.as_bytes())
        .map_err(|error| unreadable(path, &format!("not an RSA key: {error}")))
}

/// The end of central directory record of the zip in `bytes`.
fn end_of_zip(bytes: &[u8]) -> Result<signature::End, SignatureError> {
    signature::end_of_central_directory(bytes, bytes.len()).ok_or_else(|| {
        SignatureError::new("it is not a zip: no end of central directory record was found")
    })
}

/// Refuses a zip whose comment does not end exactly where the file does.
fn ends_the_file(bytes: &[u8], end: &signature::End) -> Result<(), SignatureError> {
    use std::cmp::Ordering;

    match end.end().cmp(&bytes.len()) {
        Ordering::Equal => Ok(()),
        // A comment shorter than the file is what both upstream layouts leave
        // behind, and what a reader that only skips to the comment end sees.
        Ordering::Less => Err(SignatureError::new(format!(
            "it is signed, or has {} bytes after its zip's comment",
            bytes.len() - end.end()
        ))),
        Ordering::Greater => Err(SignatureError::new(
            "its zip comment runs past the end of the file",
        )),
    }
}

fn unreadable(path: &Path, reason: &str) -> Error {
    Error::Signing(format!("cannot read {}: {reason}", path.display()))
}
