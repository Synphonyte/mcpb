//! The CMS signature a bundle's block carries.
//!
//! Everything this crate knows about CMS lives here, so an upward change in the
//! format crates (`cms`, `der`, `x509-cert`) stays local to this module. A bundle
//! carries exactly one detached `SignedData` signature over RSA PKCS#1 v1.5 with
//! SHA-256, whose authenticated attributes say what it signed.

use std::ops::Range;
use std::time::SystemTime;

use cms::builder::{SignedDataBuilder, SignerInfoBuilder};
use cms::cert::{CertificateChoices, IssuerAndSerialNumber};
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
use x509_cert::ext::pkix::{ExtendedKeyUsage, KeyUsage, SubjectKeyIdentifier};
use x509_cert::time::Time;

use crate::signature::{
    CertificateInfo, CertificateValidity, CryptoValidity, SignatureError, Unsupported,
};
use crate::signing::MIN_KEY_BITS;

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
#[derive(Debug)]
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
    let key = signer_key(certificate)?;
    let signed_at = check_attributes(info, content)?;
    check_signature(&key, info, der)?;
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
///
/// A signature names its signer by issuer and serial number, or by subject key
/// identifier, and either name has to find a certificate that has one: a
/// certificate that carries a key identifier can still be named by issuer and
/// serial number, which is what OpenSSL and most signing tools write.
fn signer_certificate<'a>(
    signed: &'a SignedData,
    info: &SignerInfo,
) -> Result<&'a Certificate, CmsError> {
    let mut named = certificates(signed).filter(|certificate| named_by(certificate, info));
    match (named.next(), named.next()) {
        (Some(only), None) => Ok(only),
        (None, _) => Err(malformed("its signer certificate is not in the block")),
        // Which key verified the signature would be a guess, so no answer is given.
        (Some(_), Some(_)) => Err(malformed(
            "more than one certificate in the block names itself the signer",
        )),
    }
}

/// Whether a certificate is the one a signature names as its signer.
fn named_by(certificate: &Certificate, info: &SignerInfo) -> bool {
    let tbs = certificate.tbs_certificate();

    match &info.sid {
        SignerIdentifier::IssuerAndSerialNumber(named) => {
            tbs.issuer() == &named.issuer && tbs.serial_number() == &named.serial_number
        }
        SignerIdentifier::SubjectKeyIdentifier(named) => tbs
            .get_extension::<SubjectKeyIdentifier>()
            .ok()
            .flatten()
            .is_some_and(|(_critical, identifier)| identifier == *named),
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

    // The signer chooses this time, so it is reported and never enforced: a
    // bundle signed on a clock that runs ahead would otherwise fail to verify on
    // every accurate machine until the two agreed.
    Ok(Some(
        time.decode_as::<Time>()
            .map_err(malformed)?
            .to_system_time(),
    ))
}

/// The signer's RSA key, which has to be one this crate's algorithm set verifies.
fn signer_key(certificate: &Certificate) -> Result<rsa::PublicKey, CmsError> {
    use rsa::traits::PublicKeyParts;

    let key = rsa_key(certificate).map_err(CmsError::Invalid)?;
    let bits = key.size() * 8;
    if bits < MIN_KEY_BITS {
        return Err(CmsError::Unsupported(Unsupported::WeakKey(bits)));
    }

    Ok(key)
}

/// Verifies the RSA signature over the authenticated attributes.
fn check_signature(key: &rsa::PublicKey, info: &SignerInfo, der: &[u8]) -> Result<(), CmsError> {
    let signed = signed_attributes(der)?;

    let signature = RsaSignature::try_from(info.signature.as_bytes()).map_err(|_| {
        malformed("its signature is not a PKCS#1 v1.5 signature this key could make")
    })?;

    VerifyingKey::<Sha256>::new(key.clone())
        .verify(&signed, &signature)
        .map_err(|_| {
            CmsError::Invalid(SignatureError::new(
                "its signature does not verify with its signer's key",
            ))
        })
}

/// The authenticated attributes as they arrived, tagged as the `SET OF` the
/// signature covers.
///
/// RFC 5652 5.4 says the signature covers the attributes as a `SET OF`, though
/// the `SignerInfo` carries them as an implicitly tagged `[0]`; the two differ in
/// their tag byte alone. What is hashed has to be the bytes the signer wrote:
/// decoding a `SetOfVec` sorts its members, so re-encoding an out-of-order set
/// would check a different encoding from the one that was signed.
fn signed_attributes(der: &[u8]) -> Result<Vec<u8>, CmsError> {
    let range = attributes_range(der)?;
    let mut attributes = der[range].to_vec();

    // The element is `[0] IMPLICIT`, `0xA0`; a `SET OF` is `0x31`. Both are
    // constructed, and the length and contents stay as they are, so the tag alone
    // moves — and it is the only byte that has to.
    match attributes.first() {
        Some(0xa0) => attributes[0] = 0x31,
        _ => return Err(malformed("its signed attributes are not tagged [0]")),
    }

    Ok(attributes)
}

/// Where in `der` the `[0] IMPLICIT` authenticated attributes are.
fn attributes_range(der: &[u8]) -> Result<Range<usize>, CmsError> {
    find_attributes(der).map_err(|error| {
        malformed(format!(
            "its signed attributes cannot be found in its encoding: {error}"
        ))
    })
}

/// The range of the authenticated attributes, found by walking the encoding.
fn find_attributes(der: &[u8]) -> Result<Range<usize>, cms::der::Error> {
    use cms::der::{Reader as _, SliceReader, Tag};

    let mut content_info = SliceReader::new(der)?;
    inside(&mut content_info, |content_info| {
        // ContentInfo ::= SEQUENCE { contentType, [0] EXPLICIT content }
        content_info.tlv_bytes()?;
        inside(content_info, |content| {
            // SignedData ::= SEQUENCE { version, digestAlgorithms,
            //                          encapContentInfo, certificates [0]?,
            //                          crls [1]?, signerInfos }
            inside(content, |signed_data| {
                signed_data.tlv_bytes()?;
                signed_data.tlv_bytes()?;
                signed_data.tlv_bytes()?;
                while Tag::peek(signed_data)?.is_context_specific() {
                    signed_data.tlv_bytes()?;
                }

                inside(signed_data, |signer_infos| {
                    // SignerInfo ::= SEQUENCE { version, sid, digestAlgorithm,
                    //                          signedAttrs [0]?, ... }
                    inside(signer_infos, |info| {
                        info.tlv_bytes()?;
                        info.tlv_bytes()?;
                        info.tlv_bytes()?;

                        let start = usize::try_from(info.position())?;
                        let attributes = info.tlv_bytes()?;
                        let end = start + attributes.len();

                        // The fields after the attributes are checked elsewhere;
                        // this walk only has to consume them.
                        while !info.is_finished() {
                            info.tlv_bytes()?;
                        }

                        Ok(start..end)
                    })
                })
            })
        })
    })
}

/// Reads the next element of `reader`, which has to hold nothing but `f`.
fn inside<'a, T>(
    reader: &mut cms::der::SliceReader<'a>,
    f: impl FnOnce(&mut cms::der::SliceReader<'a>) -> Result<T, cms::der::Error>,
) -> Result<T, cms::der::Error> {
    use cms::der::{Decode as _, Header, Reader as _};

    let header = Header::decode(reader)?;
    reader.read_nested(header.length(), f)
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
    // Issuer and serial number, always: a certificate that carries a subject key
    // identifier could be named either way, and issuer and serial number is the
    // name every CMS reader understands.
    let tbs = certificate.tbs_certificate();
    let sid = SignerIdentifier::IssuerAndSerialNumber(IssuerAndSerialNumber {
        issuer: tbs.issuer().clone(),
        serial_number: tbs.serial_number().clone(),
    });

    sign_naming(content, certificate, chain, key, signing_time, sid)
}

/// Builds the detached signature of `content`, naming its signer with `sid`.
fn sign_naming(
    content: &[u8],
    certificate: &Certificate,
    chain: &[Certificate],
    key: &rsa::PrivateKey,
    signing_time: Option<SystemTime>,
    sid: SignerIdentifier,
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

    let mut info = SignerInfoBuilder::new(sid, algorithm.clone(), &content_info, Some(&digest))
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;

    /// A test identity: the certificate and the key the fixtures hold for it.
    fn identity(certificate: &str, key: &str) -> (Certificate, rsa::PrivateKey) {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/signing");
        let mut certificates = crate::signing::read_certificates(&dir.join(certificate)).unwrap();
        assert_eq!(certificates.len(), 1);

        (certificates.remove(0), private_key(&dir.join(key)))
    }

    /// An unencrypted PKCS#8 key from a fixture.
    fn private_key(path: &std::path::Path) -> rsa::PrivateKey {
        let text = std::fs::read_to_string(path).unwrap();
        let (_label, document) = pkcs8::SecretDocument::from_pem(&text).unwrap();
        rsa::pkcs8::DecodePrivateKey::from_pkcs8_der(&document.to_bytes()).unwrap()
    }

    /// The content a test signature covers.
    const CONTENT: &[u8] = b"the bytes before the block";

    #[test]
    fn a_signer_is_found_by_either_name() {
        let (certificate, key) = identity("ski.pem", "ski.key");

        // This certificate carries a key identifier, so naming it by one is a
        // name a signer may write.
        assert!(matches!(
            SignerIdentifier::from(&certificate),
            SignerIdentifier::SubjectKeyIdentifier(_)
        ));

        // What this crate writes is issuer and serial number, which every CMS
        // reader understands.
        let block = sign(CONTENT, &certificate, &[], &key, None).unwrap();
        let parsed = parse(&block).unwrap();
        assert!(matches!(
            signer(&parsed).unwrap().sid,
            SignerIdentifier::IssuerAndSerialNumber(_)
        ));
        assert!(verify(CONTENT, &block, SystemTime::now()).is_ok());

        // A signature that names the same certificate by its identifier is just
        // as readable.
        let by_identifier = sign_naming(
            CONTENT,
            &certificate,
            &[],
            &key,
            None,
            SignerIdentifier::from(&certificate),
        )
        .unwrap();
        assert!(verify(CONTENT, &by_identifier, SystemTime::now()).is_ok());
    }

    #[test]
    fn a_key_below_the_floor_is_not_verified() {
        let (certificate, key) = identity("weak.pem", "weak.key");
        let block = sign(CONTENT, &certificate, &[], &key, None).unwrap();

        match verify(CONTENT, &block, SystemTime::now()) {
            Err(CmsError::Unsupported(Unsupported::WeakKey(1024))) => {}
            Ok(_) => panic!("a 1024-bit signer was verified"),
            Err(error) => panic!("a 1024-bit signer: {error}"),
        }
    }

    #[test]
    fn a_signing_time_in_the_future_is_an_annotation() {
        let (certificate, key) = identity("leaf.pem", "leaf.key");
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // A signer whose clock runs an hour ahead of the machine that checks it.
        let ahead = SystemTime::UNIX_EPOCH + Duration::from_secs(now + 3600);

        let block = sign(CONTENT, &certificate, &[], &key, Some(ahead)).unwrap();
        match verify(CONTENT, &block, SystemTime::now()) {
            Ok(verified) => assert_eq!(verified.signed_at, Some(ahead)),
            Err(error) => panic!("a signer whose clock runs ahead: {error}"),
        }
    }

    #[test]
    fn a_signature_covers_the_attributes_as_they_arrived() {
        let (certificate, key) = identity("leaf.pem", "leaf.key");
        let mut block = sign(CONTENT, &certificate, &[], &key, None).unwrap();
        assert!(verify(CONTENT, &block, SystemTime::now()).is_ok());

        // The same attributes, written in the other order: a `SET OF` holds the
        // same members, but the signature covers an encoding, and this is one the
        // signer did not write. Decoding a set sorts its members, so hashing the
        // re-encoding would accept this.
        let range = attributes_range(&block).unwrap();
        let attributes = swapped(&block[range.clone()]);
        block[range].copy_from_slice(&attributes);

        match verify(CONTENT, &block, SystemTime::now()) {
            Err(CmsError::Invalid(_)) => {}
            Ok(_) => panic!("re-ordered attributes verified"),
            Err(error) => panic!("re-ordered attributes: {error}"),
        }
    }

    /// The element `attributes` holds, with its members in the other order.
    fn swapped(attributes: &[u8]) -> Vec<u8> {
        use cms::der::{Decode as _, Header, Reader as _, SliceReader};

        let mut reader = SliceReader::new(attributes).unwrap();
        let header = Header::decode(&mut reader).unwrap();
        let header_len = usize::try_from(reader.position()).unwrap();
        assert_eq!(
            usize::try_from(header.length()).unwrap(),
            attributes.len() - header_len
        );

        let mut members = SliceReader::new(&attributes[header_len..]).unwrap();
        let first = members.tlv_bytes().unwrap().to_vec();
        let second = members.tlv_bytes().unwrap().to_vec();

        let mut swapped = attributes[..header_len].to_vec();
        swapped.extend_from_slice(&second);
        swapped.extend_from_slice(&first);
        while !members.is_finished() {
            swapped.extend_from_slice(members.tlv_bytes().unwrap());
        }

        assert_eq!(swapped.len(), attributes.len());
        swapped
    }
}
