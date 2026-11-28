//! The CMS signature a bundle's block carries.
//!
//! Everything this crate knows about CMS lives here, so an upward change in the
//! format crates (`cms`, `der`, `x509-cert`) stays local to this module. A bundle
//! carries exactly one detached `SignedData` signature over RSA PKCS#1 v1.5 with
//! SHA-256, whose authenticated attributes say what it signed.

use std::time::SystemTime;

use cms::builder::{SignedDataBuilder, SignerInfoBuilder};
use cms::cert::CertificateChoices;
use cms::content_info::ContentInfo;
use cms::der::asn1::{ObjectIdentifier, SetOfVec};
use cms::der::{Any, Decode, Encode};
use cms::signed_data::{EncapsulatedContentInfo, SignedData, SignerIdentifier, SignerInfo};
use cms::spki::AlgorithmIdentifierOwned;
use const_oid::db::rfc5280::ID_KP_CODE_SIGNING;
use const_oid::db::rfc5911::{
    ID_CONTENT_TYPE, ID_DATA, ID_MESSAGE_DIGEST, ID_SIGNED_DATA, ID_SIGNING_TIME,
};
use const_oid::db::rfc5912::{ID_SHA_256, RSA_ENCRYPTION, SHA_256_WITH_RSA_ENCRYPTION};
use rsa::pkcs1v15::{Signature as RsaSignature, SigningKey, VerifyingKey};
use rsa::signature::Verifier;
use sha2::{Digest, Sha256};
use x509_cert::Certificate;
use x509_cert::attr::Attribute;
use x509_cert::ext::pkix::{ExtendedKeyUsage, KeyUsage};
use x509_cert::time::Time;

use crate::signature::{
    CertificateInfo, CertificateValidity, CryptoValidity, SignatureError, Unsupported,
};

/// What a verified signature establishes.
pub(crate) struct Verified {
    /// The signer certificate, as a report names it.
    pub(crate) info: CertificateInfo,
    /// Whether that certificate was valid at the time of the check.
    pub(crate) validity: CertificateValidity,
    /// The signing time the signature claims, if it claims one.
    pub(crate) signed_at: Option<SystemTime>,
    /// The signer certificate, then its chain, for the trust check.
    pub(crate) certificates: Vec<Certificate>,
}

/// Why a signature does not verify.
pub(crate) enum CmsError {
    /// The block cannot be read as a detached CMS signature.
    Malformed(SignatureError),
    /// The signature's digest of the bytes does not match them.
    Digest,
    /// The signature does not verify, or its signer may not sign code.
    Invalid(SignatureError),
    /// A CMS feature this crate does not verify.
    Unsupported(Unsupported),
}

impl std::fmt::Display for CmsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(reason) | Self::Invalid(reason) => f.write_str(&reason.to_string()),
            Self::Digest => f.write_str(
                "the signature does not cover these bytes: its digest of them does not match",
            ),
            Self::Unsupported(what) => f.write_str(&what.to_string()),
        }
    }
}

impl From<CmsError> for CryptoValidity {
    fn from(error: CmsError) -> Self {
        match error {
            CmsError::Malformed(reason) => Self::Malformed(reason),
            CmsError::Digest => Self::Invalid(SignatureError::new(
                "the signature does not cover these bytes: its digest of them does not match",
            )),
            CmsError::Invalid(reason) => Self::Invalid(reason),
            CmsError::Unsupported(what) => Self::Unsupported(what),
        }
    }
}

/// Reads the malformed-block error of a CMS decoding failure.
fn malformed(reason: impl std::fmt::Display) -> CmsError {
    CmsError::Malformed(SignatureError::new(format!(
        "its CMS signature cannot be read: {reason}"
    )))
}

/// Verifies the signature in `der` over `content`, detached.
pub(crate) fn verify(content: &[u8], der: &[u8], at: SystemTime) -> Result<Verified, CmsError> {
    let signed = parse(der)?;
    let info = signer(&signed)?;
    check_algorithms(info)?;
    let certificate = signer_certificate(&signed, info)?;
    let signed_at = check_attributes(info, content)?;
    check_signature(certificate, info)?;
    check_code_signing(certificate).map_err(CmsError::Invalid)?;

    Ok(Verified {
        info: certificate_info(certificate)?,
        validity: validity(certificate, at),
        signed_at,
        certificates: std::iter::once(certificate)
            .cloned()
            .chain(
                certificates(&signed)
                    .filter(|other| *other != certificate)
                    .cloned(),
            )
            .collect(),
    })
}

/// The message digest a signature claims for the bytes it signed.
///
/// This reads the claim without verifying it, which is only used to name a block
/// that was signed before its zip comment was patched.
pub(crate) fn claimed_digest(der: &[u8]) -> Option<Vec<u8>> {
    let signed = parse(der).ok()?;
    let info = signer(&signed).ok()?;
    digest_attribute(info).ok()
}

/// Parses a `SignedData` out of a signature block.
fn parse(der: &[u8]) -> Result<SignedData, CmsError> {
    let info = ContentInfo::from_der(der).map_err(malformed)?;
    if info.content_type != ID_SIGNED_DATA {
        return Err(malformed(format!(
            "it holds {} rather than signed data",
            info.content_type
        )));
    }

    info.content.decode_as::<SignedData>().map_err(malformed)
}

/// The one signer a bundle's signature may have.
fn signer(signed: &SignedData) -> Result<&SignerInfo, CmsError> {
    let mut signers = signed.signer_infos.0.iter();
    match (signers.next(), signers.next()) {
        (Some(only), None) => Ok(only),
        (None, _) => Err(malformed("it names no signer")),
        (Some(_), Some(_)) => Err(CmsError::Unsupported(Unsupported::MultipleSigners)),
    }
}

/// The certificates a signature carries, signer first or not.
fn certificates(signed: &SignedData) -> impl Iterator<Item = &Certificate> {
    signed
        .certificates
        .iter()
        .flat_map(|set| set.0.iter())
        .filter_map(|choice| match choice {
            CertificateChoices::Certificate(certificate) => Some(certificate),
            CertificateChoices::Other(_) => None,
        })
}

/// Refuses everything outside RSA PKCS#1 v1.5 with SHA-256 and authenticated
/// attributes, which is what this crate writes and checks.
fn check_algorithms(info: &SignerInfo) -> Result<(), CmsError> {
    if info.digest_alg.oid != ID_SHA_256 {
        return Err(CmsError::Unsupported(Unsupported::Algorithm(format!(
            "its digest algorithm ({})",
            info.digest_alg.oid
        ))));
    }

    // `rsaEncryption` is what RFC 3279 asked for here and `sha256WithRSAEncryption`
    // is what both OpenSSL and this crate write; the digest is checked either way.
    let algorithm = info.signature_algorithm.oid;
    if algorithm != SHA_256_WITH_RSA_ENCRYPTION && algorithm != RSA_ENCRYPTION {
        return Err(CmsError::Unsupported(Unsupported::Algorithm(format!(
            "its signature algorithm ({algorithm})"
        ))));
    }

    match info.signed_attrs.as_ref().map(SetOfVec::len) {
        Some(0) | None => Err(CmsError::Unsupported(Unsupported::NoSignedAttributes)),
        Some(_) => Ok(()),
    }
}

/// The certificate the signature names by its signer identifier.
fn signer_certificate<'a>(
    signed: &'a SignedData,
    info: &SignerInfo,
) -> Result<&'a Certificate, CmsError> {
    let mut named =
        certificates(signed).filter(|certificate| SignerIdentifier::from(*certificate) == info.sid);
    match (named.next(), named.next()) {
        (Some(only), None) => Ok(only),
        (None, _) => Err(malformed("its signer certificate is not in the block")),
        // Which key verified the signature would be a guess, so no answer is given.
        (Some(_), Some(_)) => Err(malformed(
            "more than one certificate in the block names itself the signer",
        )),
    }
}

/// The attribute of `oid`, which a signature may carry only once.
fn attribute(info: &SignerInfo, oid: ObjectIdentifier) -> Result<Option<&Any>, CmsError> {
    let attributes = info
        .signed_attrs
        .as_ref()
        .expect("checked by check_algorithms");

    let mut found: Option<&Any> = None;
    for attribute in attributes.iter().filter(|attr| attr.oid == oid) {
        if found.is_some() {
            return Err(malformed(format!(
                "it carries more than one {oid} attribute"
            )));
        }
        found = Some(
            attribute
                .values
                .get(0)
                .ok_or_else(|| malformed(format!("its {oid} attribute is empty")))?,
        );
    }
    Ok(found)
}

/// The `messageDigest` attribute's value.
fn digest_attribute(info: &SignerInfo) -> Result<Vec<u8>, CmsError> {
    let digest = attribute(info, ID_MESSAGE_DIGEST)?
        .ok_or_else(|| malformed("it carries no messageDigest attribute"))?;
    let octets = digest
        .decode_as::<cms::der::asn1::OctetString>()
        .map_err(malformed)?;
    Ok(octets.as_bytes().to_vec())
}

/// Checks what the authenticated attributes say about `content`, and returns the
/// signing time if one is claimed.
fn check_attributes(info: &SignerInfo, content: &[u8]) -> Result<Option<SystemTime>, CmsError> {
    let content_type = attribute(info, ID_CONTENT_TYPE)?
        .ok_or_else(|| malformed("it carries no contentType attribute"))?
        .decode_as::<ObjectIdentifier>()
        .map_err(malformed)?;
    if content_type != ID_DATA {
        return Err(CmsError::Invalid(SignatureError::new(format!(
            "it signs {content_type} rather than ordinary data"
        ))));
    }

    if digest_attribute(info)? != Sha256::digest(content).as_slice() {
        return Err(CmsError::Digest);
    }

    let Some(time) = attribute(info, ID_SIGNING_TIME)? else {
        return Ok(None);
    };
    let signed_at = time
        .decode_as::<Time>()
        .map_err(malformed)?
        .to_system_time();
    if signed_at > SystemTime::now() {
        return Err(CmsError::Invalid(SignatureError::new(
            "it claims a signing time in the future",
        )));
    }

    Ok(Some(signed_at))
}

/// Verifies the RSA signature over the authenticated attributes.
fn check_signature(certificate: &Certificate, info: &SignerInfo) -> Result<(), CmsError> {
    let attributes = info
        .signed_attrs
        .as_ref()
        .expect("checked by check_algorithms");

    // RFC 5652 5.4: the signature covers the attributes as a `SET OF`, though
    // the `SignerInfo` carries them as an implicitly tagged `[0]`.
    let signed = attributes
        .to_der()
        .map_err(|error| malformed(format!("its signed attributes cannot be encoded: {error}")))?;

    let signature = RsaSignature::try_from(info.signature.as_bytes()).map_err(|_| {
        malformed("its signature is not a PKCS#1 v1.5 signature this key could make")
    })?;

    let key = rsa_key(certificate).map_err(CmsError::Invalid)?;
    VerifyingKey::<Sha256>::new(key)
        .verify(&signed, &signature)
        .map_err(|_| {
            CmsError::Invalid(SignatureError::new(
                "its signature does not verify with its signer's key",
            ))
        })
}

/// Refuses a signer that may not sign software.
pub(crate) fn check_code_signing(certificate: &Certificate) -> Result<(), SignatureError> {
    let tbs = certificate.tbs_certificate();

    match tbs
        .get_extension::<ExtendedKeyUsage>()
        .map_err(|error| SignatureError::new(format!("its extensions cannot be read: {error}")))?
    {
        Some((_, usage)) if usage.0.contains(&ID_KP_CODE_SIGNING) => {}
        Some(_) => {
            return Err(SignatureError::new(
                "its signer certificate may not sign software: codeSigning is not among its extended key usages",
            ));
        }
        None => {
            return Err(SignatureError::new(
                "its signer certificate may not sign software: it declares no codeSigning extended key usage",
            ));
        }
    }

    if let Some((_, usage)) = tbs
        .get_extension::<KeyUsage>()
        .map_err(|error| SignatureError::new(format!("its key usage cannot be read: {error}")))?
        && !usage.digital_signature()
    {
        return Err(SignatureError::new(
            "its signer certificate's key may not make digital signatures",
        ));
    }

    Ok(())
}

/// A certificate's public key as an RSA key.
pub(crate) fn rsa_key(certificate: &Certificate) -> Result<rsa::PublicKey, SignatureError> {
    let spki = certificate.tbs_certificate().subject_public_key_info();
    let der = spki
        .to_der()
        .map_err(|error| SignatureError::new(format!("its key cannot be read: {error}")))?;
    use rsa::pkcs8::DecodePublicKey;
    rsa::PublicKey::from_public_key_der(&der)
        .map_err(|_| SignatureError::new("its key is not an RSA key"))
}

/// What a report says about a certificate.
pub(crate) fn certificate_info(certificate: &Certificate) -> Result<CertificateInfo, CmsError> {
    let tbs = certificate.tbs_certificate();
    let der = certificate
        .to_der()
        .map_err(|error| malformed(format!("a certificate cannot be re-encoded: {error}")))?;

    Ok(CertificateInfo {
        subject: tbs.subject().to_string(),
        issuer: tbs.issuer().to_string(),
        serial: hex(tbs.serial_number().as_bytes()),
        sha256: hex(&Sha256::digest(&der)),
        self_signed: self_signed(certificate),
        valid_from: tbs.validity().not_before.to_system_time(),
        valid_to: tbs.validity().not_after.to_system_time(),
    })
}

/// Whether a certificate's validity period covers `at`.
pub(crate) fn validity(certificate: &Certificate, at: SystemTime) -> CertificateValidity {
    let validity = certificate.tbs_certificate().validity();
    if at < validity.not_before.to_system_time() {
        CertificateValidity::NotYetValid
    } else if at > validity.not_after.to_system_time() {
        CertificateValidity::Expired
    } else {
        CertificateValidity::Valid
    }
}

/// Whether the certificate is its own issuer and signs itself.
fn self_signed(certificate: &Certificate) -> bool {
    let tbs = certificate.tbs_certificate();
    if tbs.subject() != tbs.issuer()
        || certificate.signature_algorithm().oid != SHA_256_WITH_RSA_ENCRYPTION
    {
        return false;
    }

    let (Ok(key), Ok(signed), Ok(signature)) = (
        rsa_key(certificate),
        tbs.to_der(),
        RsaSignature::try_from(certificate.signature().raw_bytes()),
    ) else {
        return false;
    };

    VerifyingKey::<Sha256>::new(key)
        .verify(&signed, &signature)
        .is_ok()
}

/// Builds the detached signature of `content`, with the signer certificate first
/// in the certificate set so that every reader finds it.
pub(crate) fn sign(
    content: &[u8],
    certificate: &Certificate,
    chain: &[Certificate],
    key: &rsa::PrivateKey,
    signing_time: Option<SystemTime>,
) -> Result<Vec<u8>, CmsError> {
    let failed = |error: cms::builder::Error| {
        CmsError::Malformed(SignatureError::new(format!(
            "the signature cannot be built: {error}"
        )))
    };

    let content_info = EncapsulatedContentInfo {
        econtent_type: ID_DATA,
        econtent: None,
    };
    let algorithm = AlgorithmIdentifierOwned {
        oid: ID_SHA_256,
        parameters: None,
    };
    let digest = Sha256::digest(content).to_vec();

    let mut info = SignerInfoBuilder::new(
        SignerIdentifier::from(certificate),
        algorithm.clone(),
        &content_info,
        Some(&digest),
    )
    .map_err(failed)?;

    if let Some(at) = signing_time {
        let time = Time::try_from(at).map_err(|error| {
            CmsError::Malformed(SignatureError::new(format!(
                "the signing time {at:?} cannot be written: {error}"
            )))
        })?;
        let mut values = SetOfVec::new();
        values
            .insert(Any::encode_from(&time).map_err(malformed)?)
            .map_err(malformed)?;
        info.add_signed_attribute(Attribute {
            oid: ID_SIGNING_TIME,
            values,
        })
        .map_err(failed)?;
    }

    let signer = SigningKey::<Sha256>::new(key.clone());
    let mut signed = SignedDataBuilder::new(&content_info);
    signed.add_digest_algorithm(algorithm).map_err(failed)?;
    signed
        .add_certificate(CertificateChoices::Certificate(certificate.clone()))
        .map_err(failed)?;
    for intermediate in chain {
        signed
            .add_certificate(CertificateChoices::Certificate(intermediate.clone()))
            .map_err(failed)?;
    }
    signed
        .add_signer_info::<SigningKey<Sha256>, RsaSignature>(info, &signer)
        .map_err(failed)?;

    signed
        .build()
        .map_err(failed)?
        .to_der()
        .map_err(|error| malformed(format!("the signature cannot be encoded: {error}")))
}

/// Bytes as lowercase hexadecimal.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
