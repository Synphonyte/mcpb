//! A bundle's signature: the block appended after the zip, and what it says.
//!
//! A signature is appended, never inserted, so the zip a client reads is the
//! zip that was signed. The end of central directory record ends with the
//! zip comment's length, and that field has to cover the block for the file to
//! stay a valid zip, so signing has one ordering rule: patch the comment length
//! first, then sign the patched bytes. Upstream `mcpb` breaks that rule in both
//! of its release lines, which is why their signatures are not accepted here;
//! see `docs/signing.md`.
//!
//! ```text
//! [zip ... end of central directory]
//! MCPB_SIG_V1 | u32le(der_len) | DER CMS signature | (zeros) | MCPB_SIG_END
//! ```

use std::fmt;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::Path;
use std::time::SystemTime;

use crate::Error;

/// The marker a signature block starts with.
pub(crate) const HEADER: &[u8] = b"MCPB_SIG_V1";

/// The marker a signature block ends with.
pub(crate) const FOOTER: &[u8] = b"MCPB_SIG_END";

/// How many bytes the DER signature's little-endian length takes.
pub(crate) const LENGTH: usize = 4;

/// The end of central directory record, before its comment.
pub(crate) const END: usize = 22;

/// The most bytes a zip comment can hold, which bounds a block's length.
pub(crate) const COMMENT_MAX: usize = u16::MAX as usize;

/// How much of the end of a bundle is read to look for a block.
pub(crate) const TAIL: usize = COMMENT_MAX + HEADER.len() + LENGTH + FOOTER.len();

/// How a signature block is laid out after the DER signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// The footer follows the DER signature directly.
    Canonical,
    /// A reserved slot: zeros pad the signature out to the slot's length, so an
    /// HSM can sign bytes that never change afterwards.
    Padded,
}

impl fmt::Display for Layout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Canonical => f.write_str("canonical"),
            Self::Padded => f.write_str("padded"),
        }
    }
}

/// A layout this crate does not write, which it therefore does not verify.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForeignLayout {
    /// `mcpb` 2.1.2: the block trails a comment that ends at its header.
    Mcpb212,
    /// Upstream `main`: the comment covers the block, but the signature was made
    /// before that field was bumped, so it covers different bytes.
    UpstreamMain,
    /// A sound block whose comment length this crate does not recognize.
    Unknown,
}

impl fmt::Display for ForeignLayout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mcpb212 => f.write_str("written by mcpb 2.1.2"),
            Self::UpstreamMain => {
                f.write_str("written by upstream mcpb, which signs before declaring the block")
            }
            Self::Unknown => f.write_str("in a layout this crate does not write"),
        }
    }
}

/// What a bundle's bytes say about a signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignatureState {
    /// No signature block: no `MCPB_SIG_END` at the end of the file, or no
    /// `MCPB_SIG_V1` before it.
    None,
    /// A sound block in a layout this crate does not write. Nothing is verified.
    Foreign(ForeignLayout),
    /// A block this crate recognizes, which may still be unreadable or invalid.
    Present(Box<Signature>),
}

impl SignatureState {
    /// Whether block framing is present, whatever it says.
    pub fn has_block(&self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Whether a signature verifies, and if not, why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CryptoValidity {
    /// The CMS signature verifies over the bytes before the block, and the
    /// signer certificate may sign code.
    Valid,
    /// The signature does not verify: a digest, signature or attribute mismatch.
    Invalid(SignatureError),
    /// The block cannot be read as a detached CMS signature at all.
    Malformed(SignatureError),
    /// A CMS feature this crate does not verify.
    Unsupported(Unsupported),
}

/// What a signature asks for that this crate does not verify.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unsupported {
    /// More than one signer; a bundle has exactly one.
    MultipleSigners,
    /// No authenticated attributes, so the signature covers the content rather
    /// than the attributes this crate checks.
    NoSignedAttributes,
    /// A digest or signature algorithm outside RSA PKCS#1 v1.5 with SHA-256.
    Algorithm(String),
    /// A signer key below the 2048-bit floor, in bits.
    WeakKey(usize),
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MultipleSigners => f.write_str("it has more than one signer"),
            Self::NoSignedAttributes => f.write_str("it has no authenticated attributes to check"),
            Self::Algorithm(what) => write!(f, "{what} is not RSA PKCS#1 v1.5 with SHA-256"),
            Self::WeakKey(bits) => write!(
                f,
                "its signer key is {bits} bits, and only keys of at least 2048 are verified"
            ),
        }
    }
}

/// Why a block is malformed, or why its signature does not verify.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureError(String);

impl SignatureError {
    pub(crate) fn new(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }
}

impl fmt::Display for SignatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SignatureError {}

/// Whether the signer certificate's validity period covers the time of the check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CertificateValidity {
    Valid,
    Expired,
    NotYetValid,
    /// There is no signer certificate to ask, as in a block that cannot be read.
    Unknown,
}

impl fmt::Display for CertificateValidity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Valid => f.write_str("valid"),
            Self::Expired => f.write_str("expired"),
            Self::NotYetValid => f.write_str("not yet valid"),
            Self::Unknown => f.write_str("unknown"),
        }
    }
}

/// What trust in the signing certificate was established.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trust {
    /// No chain was built: `--no-trust`, or a platform with no code-signing
    /// policy store.
    NotEvaluated,
    /// The chain reaches a trusted anchor.
    Trusted,
    /// The chain does not reach a trusted anchor.
    Untrusted(TrustProblem),
    /// The check itself could not run.
    Error(TrustError),
}

impl fmt::Display for Trust {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotEvaluated => f.write_str("not evaluated"),
            Self::Trusted => f.write_str("trusted"),
            Self::Untrusted(problem) => write!(f, "untrusted: {problem}"),
            Self::Error(error) => write!(f, "could not be checked: {error}"),
        }
    }
}

/// Why a certificate chain does not reach a trusted anchor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrustProblem {
    /// The chain is broken, has expired certificates, or leads nowhere.
    ChainInvalid(String),
    /// A certificate in the path is signed with an algorithm the pure-Rust
    /// `--ca-file` check does not implement (it verifies RSA PKCS#1 v1.5/SHA-256).
    UnsupportedChainAlgorithm {
        certificate: String,
        algorithm: String,
    },
    /// No trusted anchor was supplied.
    NoAnchor,
}

impl fmt::Display for TrustProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChainInvalid(reason) => f.write_str(reason),
            Self::UnsupportedChainAlgorithm {
                certificate,
                algorithm,
            } => write!(
                f,
                "{certificate} is signed with {algorithm}, which the pure-Rust check does not verify"
            ),
            Self::NoAnchor => f.write_str("no trusted certificate was given"),
        }
    }
}

/// Why a trust check could not say anything about the chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrustError {
    /// The platform's verifier could not be run, or failed to run at all.
    ToolFailed { program: String, message: String },
    /// The check was asked for something it cannot do.
    Configuration(String),
}

impl fmt::Display for TrustError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ToolFailed { program, message } => write!(f, "{program} failed: {message}"),
            Self::Configuration(reason) => f.write_str(reason),
        }
    }
}

/// The signer certificate, as a report names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CertificateInfo {
    /// The distinguished name of the certificate's subject.
    pub subject: String,
    /// The distinguished name of the certificate's issuer.
    pub issuer: String,
    /// The certificate's serial number, in hexadecimal.
    pub serial: String,
    /// The SHA-256 fingerprint of the certificate's DER, in hexadecimal.
    pub sha256: String,
    /// Whether the certificate is self-signed: its issuer is its subject and its
    /// own signature verifies. A certificate property, not a trust level.
    pub self_signed: bool,
    /// The start of the certificate's validity period.
    pub valid_from: SystemTime,
    /// The end of the certificate's validity period.
    pub valid_to: SystemTime,
}

/// A signature block this crate recognizes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    /// How the block is laid out, or `None` when its framing is too broken to
    /// tell.
    pub layout: Option<Layout>,
    /// The CMS signature, and whether it verifies.
    pub crypto: CryptoValidity,
    /// The signer certificate, when one could be found and read.
    pub signer: Option<CertificateInfo>,
    /// Whether the signer certificate was valid at the time of the check.
    pub validity: CertificateValidity,
    /// The signing time the signature claims. Signer-asserted, so it is an
    /// annotation: it never extends how long a bundle verifies.
    ///
    /// Nothing here says whether the signer is trusted: that is a chain check,
    /// and it is [`crate::check_trust`]'s answer, not this report's.
    pub signed_at: Option<SystemTime>,
    /// The signer certificate, then its chain as the signature carries it. Trust
    /// needs the DER; the report above names them instead.
    #[cfg(feature = "signing")]
    pub(crate) certificates: Vec<x509_cert::Certificate>,
}

#[cfg(feature = "signing")]
impl Signature {
    /// A block whose framing says nothing about a signature.
    fn unreadable(error: SignatureError) -> Self {
        Self {
            layout: None,
            crypto: CryptoValidity::Malformed(error),
            signer: None,
            validity: CertificateValidity::Unknown,
            signed_at: None,
            #[cfg(feature = "signing")]
            certificates: Vec::new(),
        }
    }
}

/// Where a signature block's bytes are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Block {
    /// The offset of the header.
    pub(crate) start: usize,
    /// The DER signature, between the header's length and the padding.
    pub(crate) der: Range<usize>,
    /// Whether zeros pad the block out to a reserved slot.
    pub(crate) padded: bool,
}

#[cfg(feature = "signing")]
impl Block {
    /// How the block is laid out.
    pub(crate) fn layout(&self) -> Layout {
        match self.padded {
            true => Layout::Padded,
            false => Layout::Canonical,
        }
    }
}

/// What the end of a file says about a signature.
pub(crate) enum Framing {
    /// No block: no footer at the end of the file, or no header before it.
    None,
    /// Both markers are there, but they frame no signed range.
    Broken(SignatureError),
    /// A block whose signed range is known.
    Block(Block),
}

/// The signature block of `bytes`, if its markers frame one.
pub(crate) fn framing(bytes: &[u8]) -> Framing {
    // A block ends the file, so the footer of a block is the last thing in it. The
    // same bytes may also be ordinary data, as in a zip comment, which is no block
    // unless a header precedes them.
    let Some(footer) = last(bytes, FOOTER) else {
        return Framing::None;
    };
    if footer + FOOTER.len() != bytes.len() {
        // Bytes after a footer are a broken block only when that footer is outside
        // the zip. A stored entry can hold the very same markers, as a bundle
        // shipping signed fixtures does, and the zip it is packed into ends after
        // them.
        let outside = end_of_central_directory(bytes, bytes.len())
            .is_some_and(|end| footer >= end.offset + END);
        return match outside && last(&bytes[..footer], HEADER).is_some() {
            // A block that something was appended to is a broken block, not an
            // unsigned bundle: saying `unsigned` would hide that it was signed.
            true => Framing::Broken(SignatureError::new(
                "it has bytes after its signature block",
            )),
            false => Framing::None,
        };
    }

    // A CMS signature's certificates can hold the header as data, so the block is
    // the last header before the footer that frames a signed range rather than
    // simply the last header before the footer. The comment declares the block, so
    // the header is among the bytes a comment can hold.
    let after = footer.saturating_sub(TAIL);
    let mut broken = None;
    let mut from = footer;
    while let Some(start) = last(&bytes[after..from], HEADER).map(|at| at + after) {
        match block_at(bytes, start, footer) {
            Ok(block) => return Framing::Block(block),
            Err(error) => {
                if broken.is_none() {
                    broken = Some(error);
                }
            }
        }
        from = start;
    }

    match broken {
        Some(error) => Framing::Broken(error),
        None => Framing::None,
    }
}

/// The offset of the last `needle` in `bytes`.
fn last(bytes: &[u8], needle: &[u8]) -> Option<usize> {
    bytes
        .windows(needle.len())
        .rposition(|window| window == needle)
}

/// The block whose header is at `start` and whose footer ends `bytes`.
fn block_at(bytes: &[u8], start: usize, footer: usize) -> Result<Block, SignatureError> {
    let length_at = start + HEADER.len();
    let der_at = length_at + LENGTH;
    if der_at > footer {
        return Err(SignatureError::new(
            "the signature block is too short to hold its length",
        ));
    }

    let declared = u32::from_le_bytes(bytes[length_at..der_at].try_into().expect("four bytes"));
    let der_end = der_at + usize::try_from(declared).unwrap_or(usize::MAX);
    if der_end > footer || der_end < der_at {
        return Err(SignatureError::new(format!(
            "the signature block declares {declared} bytes of signature, which run past its end"
        )));
    }

    if bytes[der_end..footer].iter().any(|byte| *byte != 0) {
        return Err(SignatureError::new(
            "the signature block has bytes between its signature and its end that are not padding",
        ));
    }

    Ok(Block {
        start,
        der: der_at..der_end,
        padded: der_end < footer,
    })
}

/// The zip's end of central directory record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct End {
    /// The offset of the record.
    pub(crate) offset: usize,
    /// The length of the zip comment it declares.
    pub(crate) comment: usize,
}

impl End {
    /// The offset just past the comment the record declares.
    pub(crate) fn end(&self) -> usize {
        self.offset + END + self.comment
    }
}

/// The end of central directory record before `at`, if the zip has one.
pub(crate) fn end_of_central_directory(bytes: &[u8], at: usize) -> Option<End> {
    const SIGNATURE: &[u8] = b"PK\x05\x06";

    let mut at = at.min(bytes.len());
    loop {
        // The record sits before `at`, no further back than its own length plus
        // the longest comment it could declare.
        let from = at.saturating_sub(END + COMMENT_MAX);
        let found = bytes[from..at]
            .windows(SIGNATURE.len())
            .rposition(|window| window == SIGNATURE)?
            + from;

        // Those four bytes can be ordinary data. A record needs its own bytes,
        // so one that does not fit before `at` is not a record.
        if found + END <= at {
            let comment = u16::from_le_bytes([bytes[found + 20], bytes[found + 21]]);
            return Some(End {
                offset: found,
                comment: usize::from(comment),
            });
        }
        if found == 0 {
            return None;
        }
        at = found;
    }
}

/// Sets the end of central directory record's comment length.
pub(crate) fn set_comment(bytes: &mut [u8], end: &End, comment: u16) {
    bytes[end.offset + 20..end.offset + 22].copy_from_slice(&comment.to_le_bytes());
}

/// Whether a bundle carries a signature block, reading only its end.
pub(crate) fn has_block(path: &Path) -> Result<bool, Error> {
    Ok(!matches!(framing(&tail(path)?), Framing::None))
}

/// The last [`TAIL`] bytes of the file at `path`.
fn tail(path: &Path) -> Result<Vec<u8>, Error> {
    let mut file = fs::File::open(path).map_err(|error| Error::read(path, error))?;
    let size = file
        .metadata()
        .map_err(|error| Error::read(path, error))?
        .len();

    let start = size.saturating_sub(TAIL as u64);
    file.seek(SeekFrom::Start(start))
        .map_err(|error| Error::read(path, error))?;
    let mut bytes = Vec::with_capacity((size - start) as usize);
    file.read_to_end(&mut bytes)
        .map_err(|error| Error::read(path, error))?;
    Ok(bytes)
}

/// Removes the signature block of the bundle at `path`.
///
/// Only a block this crate writes (canonical or padded) is removed: a foreign or
/// malformed block is an error, so `unsign` never rewrites a file it does not
/// understand.
pub fn unsign(path: &Path) -> Result<(), Error> {
    let bytes = fs::read(path).map_err(|error| Error::read(path, error))?;
    let signed = without_block(&bytes).map_err(|reason| {
        Error::Signing(format!(
            "cannot remove the signature of {}: {reason}",
            path.display()
        ))
    })?;

    match signed {
        Some(unsigned) => replace(path, &unsigned),
        None => Err(Error::Signing(format!("{} is not signed", path.display()))),
    }
}

/// `bytes` without their signature block, or `None` when they have none.
///
/// Fails for a block this crate does not write, which is not removed.
pub(crate) fn without_block(bytes: &[u8]) -> Result<Option<Vec<u8>>, SignatureError> {
    let block = match framing(bytes) {
        Framing::None => return Ok(None),
        Framing::Broken(error) => return Err(error),
        Framing::Block(block) => block,
    };

    let end = end_of_central_directory(bytes, block.start).ok_or_else(|| {
        SignatureError::new("it has no end of central directory record before the block")
    })?;
    if end.end() != bytes.len() {
        return Err(SignatureError::new(
            "it was not written by this crate: the zip's comment does not cover its signature block",
        ));
    }

    let comment = end
        .comment
        .checked_sub(bytes.len() - block.start)
        .ok_or_else(|| {
            SignatureError::new("its zip comment is shorter than its signature block")
        })?;
    let comment = u16::try_from(comment)
        .map_err(|_| SignatureError::new("its zip comment would no longer fit in its record"))?;

    let mut unsigned = bytes[..end.offset + END + usize::from(comment)].to_vec();
    set_comment(&mut unsigned, &end, comment);
    Ok(Some(unsigned))
}

/// Replaces the file at `path` with `bytes`, or leaves it as it was.
///
/// The file a symlink names is replaced, not the link, and the result keeps the
/// permissions of the file it replaces: signing a bundle completes a file the
/// caller already had, rather than writing a new one.
pub(crate) fn replace(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let temp = path.with_extension(format!("{}.sig.tmp", std::process::id()));

    let written = write(&temp, bytes)
        .and_then(|()| copy_permissions(&path, &temp))
        .and_then(|()| fs::rename(&temp, &path).map_err(Error::from));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

/// Writes `bytes` to `path`, and waits until they reach the disk.
///
/// The rename that publishes them is only atomic if the bytes are already
/// written: a machine that loses power between the two must find one file or the
/// other, never a short one.
fn write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let mut file = fs::File::create(path).map_err(Error::write)?;
    file.write_all(bytes).map_err(Error::write)?;
    file.sync_all().map_err(Error::write)
}

/// Gives `to` the permissions `from` has, when `from` is there to ask.
fn copy_permissions(from: &Path, to: &Path) -> Result<(), Error> {
    let Ok(metadata) = fs::metadata(from) else {
        return Ok(());
    };
    fs::set_permissions(to, metadata.permissions()).map_err(Error::write)
}

/// What the file at `path` says about its signature, checked now.
#[cfg(feature = "signing")]
pub fn signature(path: &Path) -> Result<SignatureState, Error> {
    signature_at(path, SystemTime::now())
}

/// What the file at `path` says about its signature, checked at `at`.
///
/// This reads the file and nothing else: a bundle whose files were damaged by
/// tampering is still reported as signed and invalid, which is what a caller has
/// to be told. [`crate::Bundle::signature`] is the same question about a bundle
/// that was read back.
#[cfg(feature = "signing")]
pub fn signature_at(path: &Path, at: SystemTime) -> Result<SignatureState, Error> {
    let bytes = fs::read(path).map_err(|error| Error::read(path, error))?;
    Ok(state(&bytes, at))
}

/// What the bundle in `bytes` says about its signature.
///
/// `at` is when the signer certificate has to be valid: the current time, or the
/// time a caller asks about with `--at`.
#[cfg(feature = "signing")]
pub(crate) fn state(bytes: &[u8], at: SystemTime) -> SignatureState {
    let block = match framing(bytes) {
        Framing::None => return SignatureState::None,
        Framing::Broken(error) => {
            return SignatureState::Present(Box::new(Signature::unreadable(error)));
        }
        Framing::Block(block) => block,
    };

    let Some(end) = end_of_central_directory(bytes, block.start) else {
        return SignatureState::Present(Box::new(Signature::unreadable(SignatureError::new(
            "no zip precedes the block: it has no end of central directory record",
        ))));
    };
    if end.offset + END > block.start {
        return SignatureState::Present(Box::new(Signature::unreadable(SignatureError::new(
            "the zip's end of central directory record overlaps the signature block",
        ))));
    }
    if end.end() > bytes.len() {
        return SignatureState::Present(Box::new(Signature::unreadable(SignatureError::new(
            "the zip's comment runs past the end of the file",
        ))));
    }

    // A layout this crate does not write is reported as such and never verified:
    // neither of upstream's layouts covers the bytes it delivers, so nothing this
    // crate could check would be the signature it is named after.
    if end.end() != bytes.len() {
        return SignatureState::Foreign(match end.end() == block.start {
            true => ForeignLayout::Mcpb212,
            false => ForeignLayout::Unknown,
        });
    }

    let content = &bytes[..block.start];
    let der = &bytes[block.der.clone()];
    let present = match crate::cms::verify(content, der, at) {
        Ok(verified) => Signature {
            layout: Some(block.layout()),
            crypto: CryptoValidity::Valid,
            signer: Some(verified.info),
            validity: verified.validity,
            signed_at: verified.signed_at,
            certificates: verified.certificates,
        },
        // Upstream `main` declares the block in the zip's comment but signed the
        // bytes that had the shorter, pre-block comment length. Those bytes are
        // still inside the delivered file, so that one misordering is
        // recognizable by name instead of only being refused as invalid.
        Err(crate::cms::CmsError::Digest)
            if crate::cms::claimed_digest(der)
                .zip(digest_before_block(
                    content,
                    &end,
                    bytes.len() - block.start,
                ))
                .is_some_and(|(claimed, pre_block)| claimed == pre_block) =>
        {
            return SignatureState::Foreign(ForeignLayout::UpstreamMain);
        }
        Err(error) => Signature {
            layout: Some(block.layout()),
            crypto: error.into(),
            signer: None,
            validity: CertificateValidity::Unknown,
            signed_at: None,
            certificates: Vec::new(),
        },
    };

    SignatureState::Present(Box::new(present))
}

/// SHA-256 of the bytes before a block, as they were before the block was
/// declared: the end of central directory record's comment length still holds
/// `comment`.
#[cfg(feature = "signing")]
fn digest_before_block(content: &[u8], end: &End, block: usize) -> Option<[u8; 32]> {
    use sha2::{Digest, Sha256};

    let comment = u16::try_from(end.comment.checked_sub(block)?).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(&content[..end.offset + 20]);
    hasher.update(comment.to_le_bytes());
    hasher.update(&content[end.offset + END..]);
    Some(hasher.finalize().into())
}
