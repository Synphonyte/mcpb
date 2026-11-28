# Signing `.mcpb` bundles

**Status:** implemented in `src/signature.rs`, `src/cms.rs`, `src/signing.rs` and
`src/trust.rs`, with tests in `tests/signing.rs` and `tests/cli.rs`. This document
is the reference for that code: where the two differ, the code is wrong.

The binary needs the `signing` and `trust` features, which are on by default; a
library user that turns them off packs exactly as before.

## 1. Goal

Add signing, verification and removal of signatures to the `mcpb` crate and CLI
without Node, OpenSSL bindings or any external crypto runtime.

"Correct" means a bundle that:

1. is a valid ZIP, so strict readers such as yauzl (Claude Desktop) and adm-zip
   accept it;
2. carries a signature covering exactly the bytes delivered on disk;
3. uses the same block framing as the official `mcpb` CLI, so either tool can
   read the other's output once upstream verification works;
4. reports absence, malformed data, invalid signatures, certificate expiry,
   untrusted chains and trusted signatures as separate facts.

## 2. Format and the one ordering rule

An `.mcpb` bundle is a ZIP. A signature is appended after the ZIP:

```text
[zip bytes ... End of Central Directory]
MCPB_SIG_V1                    11-byte ASCII marker
u32 little-endian              length of the DER signature
DER CMS SignedData             detached; signer certificate and chain included
MCPB_SIG_END                   12-byte ASCII footer
```

The ZIP End of Central Directory (EOCD) record ends with a 2-byte
`comment_length` at offset 20. For the file to remain a valid ZIP, that field
must equal the number of bytes after the EOCD, i.e. the signature block. Signing
therefore has one rule: **patch `comment_length` first, then sign the patched
bytes**. Upstream breaks it by patching after signing (`main`) or not at all
(release 2.1.2), producing files that are either zip-invalid or signed over
different bytes. This crate does not reproduce either behavior.

## 3. Decisions

| Area | Decision |
| --- | --- |
| Crypto | Pure Rust: `cms`, `rsa 0.10`, `sha2`, `x509-cert`, `pkcs8`/`pem-rfc7468`. No `openssl` crate or subprocess. |
| Trust | macOS and Windows use OS tooling. Linux does not evaluate trust by default. `--ca-file` supplies anchors and uses a pure-Rust path check on every platform. |
| Legacy layouts | Not supported. Only the canonical and reserved-slot layouts are recognized. Bundles from `mcpb` 2.1.2 or current upstream `main` are reported as unsupported with the likely producer named. |
| API naming | `is_signed()` is removed; `has_signature_block()` reports framing presence, `signature()` reports the full state. |
| Certificate expiry | A separate `CertificateValidity` axis. Expired bundles fail `verify` by default; `--allow-expired` downgrades. No trusted timestamp exists in the format; `signingTime` is only an annotation. |
| Self-signed | Fails `verify` by default (exit 1); `--allow-self-signed` permits it. `--no-trust` is the explicit crypto-only mode. |
| HSM / external signing | Part of the first implementation: `prepare-for-signing` and `apply-signature` with a zero-padded reserved slot. |
| Algorithms (v1) | RSA PKCS#1 v1.5 with SHA-256, keys >= 2048 bits, exactly one signer. No PSS, SHA-1, ECDSA or multi-signer. |

## 4. What the crate writes

### 4.1 Canonical signature (local key)

```text
patched zip (comment_length = old_comment + block_len)
MCPB_SIG_V1 | u32le(der_len) | DER | MCPB_SIG_END
```

`sign`:

1. Reads the input and locates the EOCD. Refuses if missing or if a block
   already exists, unless `--force` first removes a block the zip's comment
   declares (see `unsign`, §7). A block the comment does not declare, or one
   whose framing is broken, is refused rather than stripped.
2. Validates certificate and key: key matches certificate, certificate valid at
   signing time, `codeSigning` EKU, `digitalSignature` key usage if present, RSA
   >= 2048 bits.
3. Signs once to learn the block length, patches
   `comment_length = existing + block_len` (error above 65535), signs again, and
   repeats until the block length is stable (at most four passes; RSA length is
   fixed).
4. Verifies the result in-process, then replaces the file atomically
   (same-directory temp file + rename). A failure leaves the input untouched.

The signer certificate is added to the CMS `certificates` set before the supplied
chain. A `SET OF` is DER-sorted, so that order is not the order on disk, and
nothing relies on it: a certificate equal to the signer's is dropped from the
chain so that `SignerInfo.sid` names exactly one certificate, and verification
finds the signer by that name. Signing inputs: PEM or DER PKCS#8, encrypted
PKCS#8 or PKCS#1 RSA. A passphrase comes from `--passphrase-file`, the
`MCPB_PASSPHRASE` variable or one line of standard input, never argv; the prompt
is echoed, because reading a terminal without echo would need a dependency the
crate does not take. Keys are zeroized on drop.

### 4.2 Reserved slot (HSM / external signer)

1. `prepare-for-signing <bundle> [--slot 16384]`: refuses already-signed or
   prepared input; requires `4096 <= slot <= 65535`; appends `slot` zero bytes
   and sets `comment_length = slot`, writes atomically. The slot has to be the
   whole comment, so a bundle that already has a comment cannot be prepared:
   signing it normally, which keeps the comment inside the signed range, is the
   only layout that carries both.
2. An external signer produces a detached DER CMS signature over the bytes before
   the slot: the zip, which is the prepared file without its last `slot` bytes.
   That is the range every signature covers, canonical or padded, and
   `prepare-for-signing` prints its length so the signer cannot get it wrong.
3. `apply-signature <bundle> --signature sig.der`: requires an all-zero slot;
   encodes `MCPB_SIG_V1 || u32le(der_len) || DER` and zero-pads to exactly
   `slot` bytes; verifies the result; only then writes atomically. Oversized
   signatures and digest mismatches leave the file untouched.

Verification treats canonical and padded blocks alike: footer at EOF, header
before it, DER length in bounds, bytes between DER end and footer all zero.
`Layout::Padded` records the padding.

### 4.3 Library surface

```rust
pub struct SigningKey {
    // certificate, private key and optional chain, loaded from PEM or DER
}

impl BundleSpec {
    /// Packs, signs and writes atomically; equivalent to pack followed by sign.
    pub fn pack_signed(&self, path: &Path, key: &SigningKey) -> Result<(), Error>;
}

/// Signs an existing bundle in place (atomic rename).
pub fn sign(path: &Path, key: &SigningKey) -> Result<(), Error>;

/// Removes a canonical or padded signature; fails on foreign or malformed blocks.
pub fn unsign(path: &Path) -> Result<(), Error>;
```

`pack` and `pack_signed` keep the current atomic, reproducible behavior; a
signature necessarily changes the bytes, so `pack_signed` is reproducible only
when the caller supplies a fixed signing time.

## 5. Verification model

The report keeps block layout, crypto validity, certificate validity and trust
as separate dimensions:

```rust
pub enum Layout { Canonical, Padded }

pub enum ForeignLayout {
    Mcpb212,        // undeclared trailing block (release 2.1.2)
    UpstreamMain,   // comment covers the block, signature predates the bump
    Unknown,        // sound block, unrecognized comment-length relation
}

pub enum SignatureState {
    None,                            // no signature block
    Foreign(ForeignLayout),          // sound block in an unsupported layout
    Present(Signature),
}

pub enum CryptoValidity {
    Valid,
    Invalid(SignatureError),      // digest, signature or attribute mismatch
    Malformed(SignatureError),    // CMS/DER or required-field error
    Unsupported(Unsupported),     // multi-signer, EC/PSS or unknown digest
}

pub enum CertificateValidity { Valid, Expired, NotYetValid, Unknown }

/// Whether the chain that signed it reaches an anchor: `check_trust`'s answer,
/// not part of the report above, because the library never makes that decision
/// by itself.
pub fn check_trust(signature: &Signature, at: SystemTime, anchors: Option<&Path>) -> Trust;

pub enum Trust {
    NotEvaluated,
    Trusted,
    Untrusted(TrustProblem),      // bad link, untrusted root, unsupported chain alg
    Error(TrustError),            // verifier tool missing or crashed
}

pub enum TrustProblem {
    ChainInvalid(String),
    UnsupportedChainAlgorithm { certificate: String, algorithm: String },
    NoAnchor,
}

pub enum TrustError {
    ToolFailed { program: String, message: String },
    Configuration(String),
}

pub struct CertificateInfo {
    pub subject: String,
    pub issuer: String,
    pub serial: String,
    pub sha256: String,
    pub self_signed: bool,        // certificate property, not a trust level
    pub valid_from: SystemTime,   // the window `CertificateValidity` judges
    pub valid_to: SystemTime,
}

pub struct Signature {
    pub layout: Option<Layout>,   // None when framing is too broken to tell
    pub crypto: CryptoValidity,   // the CMS signature only
    pub signer: Option<CertificateInfo>,
    pub validity: CertificateValidity,
    pub signed_at: Option<SystemTime>,  // the signer's claim; it extends nothing
}
```

```rust
impl Bundle {
    /// True when block framing is present; does not mean the signature is valid.
    pub fn has_signature_block(&self) -> bool;

    /// Err = I/O only. A broken or foreign block is still `Present`/`Foreign`, so
    /// callers can explain what is wrong instead of guessing.
    pub fn signature(&self) -> Result<SignatureState, Error>;
    pub fn signature_at(&self, at: SystemTime) -> Result<SignatureState, Error>;
}

/// The same question about a file, without reading it back as a bundle: a bundle
/// whose files were damaged by tampering is still "signed and invalid".
pub fn signature(path: &Path) -> Result<SignatureState, Error>;
pub fn signature_at(path: &Path, at: SystemTime) -> Result<SignatureState, Error>;
```

`CryptoValidity` describes only the CMS signature and its binding to the
signer certificate, which includes whether that certificate may sign software:
a signer without the `codeSigning` extended key usage is `Invalid`, not a trust
result, because the bundle is never a valid code signature in the first place.
Certificate-chain construction is a trust result: a bad link, an untrusted root,
or a chain signature our provider cannot verify is `Trust::Untrusted(...)`,
never `CryptoValidity::Unsupported`. A foreign signature-block layout is a
`SignatureState::Foreign`, not a crypto failure.

### 5.1 Classifying a block

Let `n` be the file length, `h` the offset of the `MCPB_SIG_V1` header, `e`
the offset of the EOCD record, `c` the EOCD comment length, and `b = n - h`
the block length. A block is recognized only when `MCPB_SIG_END` ends the
file and an `MCPB_SIG_V1` header precedes it; one marker alone is not evidence
of a signature.

| Condition | Result |
| --- | --- |
| no `MCPB_SIG_END` at EOF, or no `MCPB_SIG_V1` before it | `None` |
| a footer after the EOCD but not at the end of the file, with a header before it | `Present(Malformed)` (bytes were appended after signing) |
| a footer before the EOCD, or no footer at EOF | `None` (the markers are zip data, not a block) |
| both markers present, but the length overruns the bytes before the footer, DER does not parse, non-zero bytes between DER end and footer, or no EOCD with `e+22 <= h` | `Present(Malformed)` |
| `e+22+c > n` (comment overruns the file) | `Present(Malformed)` |
| `e+22+c == n`, CMS parses but has more than one signer, an algorithm outside RSA-SHA256, or a signer key under 2048 bits | `Present(Unsupported)` |
| `e+22+c == n` and the stored content digest matches | `Present(Valid)` or `Present(Invalid)` from the signature check |
| `e+22+c == n`, stored digest fails, pre-bump digest with `c-b` matches | `Foreign(UpstreamMain)` |
| `e+22+c == n`, stored digest fails, pre-bump fails or underflows | `Present(Invalid)` |
| `e+22+c == h` | `Foreign(Mcpb212)` |
| otherwise (`e+22+c < h`, or between `h` and `n`) | `Foreign(Unknown)` |

`Foreign` never carries a crypto verdict: a layout we do not support is not
verified, and the caller is told either the producer or that the convention is
unknown. A structurally sound block is never `Malformed` only because its
layout is unsupported, and a block whose framing or CMS cannot be parsed is
never `Foreign`. `has_signature_block()` is true for `Present` and `Foreign`,
false for `None`.

A block lives in the region the zip's end record opens, so only a footer there
can be one: the markers are ordinary bytes anywhere else. A stored entry that
holds a signed bundle carries a footer and a header of its own, and the zip it
is packed into ends after them, which makes that bundle unsigned rather than
broken — and signable, since there is no block to remove first. A file that
ends with no record near enough to find has no such record to ask: the markers
alone decide, so bytes appended to a block are still reported as a broken block
however many there are.

Signature checks, in order:

- footer exactly at EOF, header before it, no trailing bytes;
- DER parses as CMS `SignedData` with exactly one `SignerInfo`, detached
  `id-data` content;
- digest SHA-256, signature RSA PKCS#1 v1.5, signer key at least 2048 bits;
- signer certificate located by `sid`: issuer and serial number, or subject key
  identifier. Either name finds the certificate, whichever the certificate
  carries; signing writes issuer and serial number, which every CMS reader reads;
- digest of the bytes before the header equals the `messageDigest` attribute;
- RSA signature validates over the authenticated attributes re-tagged as
  `SET OF` (the `[0] IMPLICIT` tag becomes `0x31`), hashed as they arrived: a
  decoder may reorder a set, and the signature covers the encoding the signer
  wrote;
- `contentType` is `id-data`. `signingTime`, if present, is reported and not
  enforced: it is the signer's claim, and a signer whose clock runs ahead still
  verifies.

Certificate checks, independent of chain trust:

- `digitalSignature` key usage if present, `codeSigning` EKU present;
- certificate validity against the verification time (default: now;
  `--at <time>` for tests);
- self-signed determined from the certificate itself (issuer equals subject and
  the self-signature verifies), independent of trust.

Certificate expiry deserves emphasis: MCPB has no trusted timestamp or
counter-signature. A bundle therefore stops verifying when its certificate
expires. `signingTime` is chosen by the signer and cannot extend trust; it is
reported as an annotation, never used to override expiry, and a time in the
future is not a failure. `Expired` fails by default and is downgraded only by
`--allow-expired`.

## 6. Trust

Both platform tools judge the chain at a time: `--at` when the caller names one,
otherwise now. `--allow-expired` moves that time to the nearest moment the
signer certificate covers, so the flag is not undone by a chain builder refusing
the condition the flag accepted.

- **macOS:** `security verify-cert -c leaf.pem -c intermediate1.pem ... -p codeSign
  -d <YYYY-MM-DD-hh:mm:ss>` — one `-c` per certificate, leaf first, written to a
  temporary directory as PEM because that is what `security` reads, and `-d` in
  GMT because that is how it is told when to judge them.
- **Windows:** PowerShell `X509Chain` with `ApplicationPolicy` OID
  `1.3.6.1.5.5.7.3.3`, intermediates in `ExtraStore`, revocation left at the
  platform's `Online` setting and `ChainPolicy.VerificationTime` set from the
  time. The certificates go to the same temporary directory as DER, the one form
  `X509Certificate2`'s file constructor reads, and the check itself is a script
  file passed with `-File` so that neither the script nor the paths have to be
  quoted into the other. The script exits 2 when the chain does not build, so any
  other non-zero exit is the check itself failing.
- **Linux:** no trust evaluation by default. There is no code-signing policy
  store; the distro bundle is unrelated system CA policy. `--ca-file <pem>`
  enables a pure-Rust path check with `rustls-webpki`, requiring the
  `codeSigning` EKU and using the CMS intermediates. The same flag works on
  every platform for deterministic CI.
- A trust tool that cannot launch, or that exits with anything but its
  "not trusted" code, is a trust **error**, not `untrusted` and not `unsigned`.
  `security` has one failure code, so on macOS a usage failure is
  indistinguishable from an untrusted chain and is reported as the latter.
- `--no-trust` skips chain evaluation and self-signed classification, reporting
  `Trust::NotEvaluated`; mutually exclusive with `--ca-file`.

Self-signed bundles are rejected by default on every platform (exit 1) unless
`--allow-self-signed` or `--no-trust` is given, and that test comes before the
chain is consulted: a `--ca-file` that names the signer as its own anchor, or a
platform store that trusts it, still leaves a certificate speaking for itself,
so it is refused until `--allow-self-signed`. On Linux without `--ca-file`, a
non-self-signed signer is reported as `Trust::NotEvaluated`: plain `mcpb verify`
exits 0 but prints a warning that the publisher was not established, the same
outcome as `--no-trust` for that bundle. `--no-trust` is the explicit mode that
bypasses both the chain check and the self-signed gate.

### 6.1 Pure-Rust signature verification for webpki

`rustls-webpki` defaults to no signature provider; `ring` and `aws-lc-rs` are
opt-in features and are not enabled. The crate implements the
`rustls_pki_types::SignatureVerificationAlgorithm` trait for RSA PKCS#1 v1.5
with SHA-256, using the same `rsa 0.10` dependency as CMS verification:

```rust
#[derive(Debug)]
struct RsaPkcs1Sha256;

impl SignatureVerificationAlgorithm for RsaPkcs1Sha256 {
    fn public_key_alg_id(&self) -> AlgorithmIdentifier {
        alg_id::RSA_ENCRYPTION
    }

    fn signature_alg_id(&self) -> AlgorithmIdentifier {
        alg_id::RSA_PKCS1_SHA256
    }

    fn verify_signature(
        &self,
        public_key: &[u8], // raw subjectPublicKey (RSAPublicKey DER)
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), InvalidSignature> {
        let key = RsaPublicKey::from_pkcs1_der(public_key)
            .map_err(|_| InvalidSignature)?;
        key.verify(
            Pkcs1v15Sign::new::<Sha256>(),
            &Sha256::digest(message),
            signature,
        )
        .map_err(|_| InvalidSignature)
    }
}
```

webpki is called with this provider and
`KeyUsage::required_if_present(&[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x03])`,
the OID value of the `codeSigning` EKU, plus trust anchors built from `--ca-file`
with `anchor_from_trusted_cert`. No C or assembly is linked for trust.

`required_if_present` rather than `required` is deliberate: webpki applies that
policy to every certificate in the path, and a real intermediate certificate
usually declares no extended key usage at all, which `required` would refuse. The
signer's own `codeSigning` EKU is required unconditionally one step earlier, when
its signature is verified, so a signer that declares no EKU is still refused; an
intermediate that declares one must include `codeSigning`.

Limitation: in `--ca-file` mode every certificate signature in the path must be
RSA PKCS#1 v1.5/SHA-256, with a key of at least 2048 bits. The path is built
first, so a certificate in the CMS set that the path does not use says nothing
about the chain. An ECDSA-signed intermediate yields
`Trust::Untrusted(TrustProblem::UnsupportedChainAlgorithm)`, with the offending
certificate and algorithm in the message. It is a trust result, not a failure
of the bundle signature.

## 7. CLI and exit codes

```sh
mcpb sign bundle.mcpb --cert cert.pem --key key.pem [--chain ca.pem ...] [--force]
mcpb verify bundle.mcpb [--no-trust | --ca-file ca.pem] [--allow-self-signed] [--allow-expired]
                        [--at <RFC 3339 time | Unix seconds>]
mcpb unsign bundle.mcpb
mcpb info bundle.mcpb
mcpb prepare-for-signing bundle.mcpb [--slot 16384]
mcpb apply-signature bundle.mcpb --signature sig.der
```

| Crypto | Certificate validity | Trust | `verify` exit |
| --- | --- | --- | --- |
| no block | — | — | 1 (`unsigned`) |
| foreign layout | — | — | 1 (`unsupported`) |
| malformed / unsupported / invalid | — | — | 1 |
| valid | expired or not yet valid | any, chain included | 1; `--allow-expired` -> 0 |
| valid | valid | self-signed signer, any chain | 1; `--allow-self-signed` or `--no-trust` -> 0 |
| valid | valid | untrusted chain | 1 |
| valid | valid | trust error (tool failed) | 1 |
| valid | valid | trusted | 0 |
| valid | valid | not evaluated (Linux default, non-self-signed) | 0, warning |
| valid | valid | not evaluated (`--no-trust`) | 0 |

Rows apply top to bottom: the self-signed signer test precedes the trust state,
so Linux's implicit `NotEvaluated` and a `--ca-file` anchor both still reject a
self-signed bundle, while `--no-trust` bypasses that gate. The expiry row applies
whatever the trust check says, because `--allow-expired` also moves the chain's
judgement into the signer certificate's validity window.

`info` always exits 0 and prints block presence, layout, crypto status,
publisher, issuer, validity window, fingerprint and trust with the reason it was
not evaluated.

`unsign` removes a block the zip's comment declares — canonical, padded, or the
upstream-`main` layout, whose block is the comment — and restores the bytes that
were signed, byte for byte. A block the comment does not declare, as `mcpb`
2.1.2 leaves it, or one whose framing is broken, is an error: rewriting those
bytes would be a guess, not a removal.

## 8. Dependencies and versions

```toml
[[bin]]
name = "mcpb"
path = "src/main.rs"
required-features = ["signing", "trust"]

[features]
default = ["signing", "trust"]
signing = ["dep:cms", "dep:const-oid", "dep:pkcs8", "dep:rsa",
           "dep:sha2", "dep:x509-cert", "dep:zeroize"]
trust   = ["signing", "dep:rustls-pki-types", "dep:rustls-webpki"]

[dependencies]
cms              = { version = "=0.3.0-rc.2", optional = true,
                     default-features = false, features = ["std", "builder"] }
const-oid        = { version = "0.10", optional = true, features = ["db"] }
pkcs8            = { version = "0.11", optional = true,
                     features = ["encryption", "pem"] }
rsa              = { version = "=0.10.0", optional = true,
                     features = ["pkcs1v15", "pkcs5"] }
rustls-pki-types = { version = "=1.16.0", optional = true }
rustls-webpki    = { version = "=0.103.17", optional = true,
                     default-features = false, features = ["std"] }
sha2             = { version = "0.11", optional = true }
x509-cert        = { version = "0.3", optional = true,
                     default-features = false, features = ["pem", "std"] }
zeroize          = { version = "1", optional = true }
```

`trust` implies `signing` because the `--ca-file` path check uses the same RSA
verifier for certificate signatures; a trust-only build is not useful without
it. Pack-only users disable both and keep today's dependency set, but they build
no `mcpb` binary: the CLI signs, so it requires both features.

- `cms 0.3.0-rc.2` is a release candidate: pinned exactly, with `builder` for CMS
  construction and `std` for parsing. Move to the exact `0.3` release when one is
  published.
- `rsa 0.10.0` is a stable release (published 2026-11-08, not yanked, MSRV 1.87),
  pinned the same way. `rsa 0.9` is excluded by RUSTSEC-2023-0071.
- `rustls-webpki` is used with `default-features = false`, so neither `ring` nor
  `aws-lc-rs` is linked; `rustls-pki-types` provides the signature algorithm
  trait the RustCrypto verifier implements.
- `const-oid` is a direct dependency for the OID constants (`id-data`,
  `contentType`, `messageDigest`, `signingTime`, `codeSigning`, the algorithms);
  `zeroize` keeps a key file's bytes out of freed memory.
- Other RustCrypto crates follow the lockfile. `deny.toml` and the `deny` job in
  CI check advisories, bans, licenses and sources on every pull request;
  grouping RustCrypto updates in Dependabot is still to be done.
- All CMS contact stays inside `src/cms.rs`. If the RC API churns, the fallback
  is the small DER subset built from `der`/`x509-cert`, already in the tree — not
  `rsa 0.9` and not OpenSSL.

## 9. What was built

1. **Block format.** `src/signature.rs`: framing (footer at the end of the file,
   the last header that frames a signed range), bounded EOCD search, canonical
   and padded parse, `unsign` byte for byte, `has_signature_block`, and the
   classification of §5.1.
2. **CMS.** `src/cms.rs`: parse, single signer, signer certificate by `sid`,
   SHA-256 and RSA PKCS#1 v1.5, digest and attribute checks, certificate checks,
   and the builder that writes the detached signature.
3. **Canonical signer.** `src/signing.rs`: identity loading and validation, the
   fixed-point patch/sign loop, an in-process check of the result, atomic
   writes, `sign`/`sign_with`/`pack_signed`.
4. **Reserved slot.** `prepare_for_signing` / `apply_signature`, padded
   verification and the CLI output that names the signed range.
5. **Trust.** `src/trust.rs`: `security verify-cert` on macOS, `X509Chain` on
   Windows, `--ca-file` with the RustCrypto webpki provider everywhere, and
   `--no-trust`; `mcpb verify` exit codes and output.
6. **Docs and watch.** The README signing section and the Claude Desktop caveat
   are written, `src/node/sign.ts` and `CLI.md` are watched in
   `.github/upstream-specs.json`, and CI runs `cargo deny check` against
   `deny.toml`. Grouping RustCrypto updates in Dependabot remains.

## 10. Tests and acceptance

`cargo test` covers the format, the crypto and the CLI. What it pins:

- `sign -> verify` reports crypto valid; `sign -> unsign` is byte-identical to the
  input, and a zip comment of the original bundle comes back unchanged.
- The `zip` crate, an independent reader, reads the signed bundle and its
  manifest. `openssl_accepts_the_signature_it_writes` has OpenSSL verify the DER
  block over the bytes before it, and the `prepare`/`apply` test has OpenSSL sign
  for an external signer in the first place. Both skip, with a message, on a
  machine without `openssl`.
- Tampering with a byte of the zip is `Invalid`; a byte of the signature, its
  declared length or the block framing is never `Valid`; a swapped block from
  another bundle is `Invalid`; a byte appended after the footer is `Malformed`,
  never `None`.
- A changed comment length leaves a sound block in no layout this crate writes,
  so it is `Foreign(Unknown)`.
- `upstream-212.mcpb` and `upstream-main.mcpb` (generated by
  `tests/fixtures/signing/upstream.sh` with OpenSSL) are `Foreign(Mcpb212)` and
  `Foreign(UpstreamMain)`, not verified. OpenSSL itself fails to verify the
  second over the delivered bytes and succeeds over the pre-bump bytes, which is
  the misordering being named.
- HSM flow: prepare, sign the bytes before the slot with `openssl cms -sign`,
  apply, verify. Preparing twice, applying to an unprepared bundle, a slot too
  small and a signature over other bytes all fail without touching the file.
- Self-signed: `verify` exits 1 by default, 0 with `--allow-self-signed`, 0 with
  `--no-trust`. Expiry: a certificate that expired is refused at signing time and
  reported `Expired` for a bundle signed while it was valid, with `--at`
  reproducing that window.
- Trust: `--ca-file tests/fixtures/signing/ca.pem` validates an RSA-SHA256 chain,
  including one through an intermediate carried in the signature; an anchor that
  did not sign it is `Untrusted`; the ECDSA-signed link is
  `Trust::Untrusted(TrustProblem::UnsupportedChainAlgorithm)`, not a crypto
  failure; Linux without `--ca-file` reports `NotEvaluated` and still refuses a
  self-signed signer.
- Block classification also covers a marker in a zip comment (`None`), non-zero
  padding, a truncated block and a missing end of central directory.
- Key/certificate mismatch, missing EKU, RSA under 2048 bits, an encrypted key
  with the wrong passphrase and a comment that cannot hold the signature: each is
  a clear error and writes nothing.
- Double signing is refused; `--force` replaces a block this crate wrote only.
- `pack_signed` writes a verifiable bundle and leaves no file when signing fails.

CI still to add: the Windows `SignedCms.CheckSignature(true)` equivalent of the
OpenSSL check. CI on Linux, macOS and Windows (x64 and arm64) runs the rest,
including the macOS `security` and Windows `X509Chain` paths, and `cargo deny
check` runs the advisory, license, ban and source checks.

## 11. Out of scope

- Certificate generation; users supply certificate and key.
- Trusted timestamps/counter-signatures; CRL/OCSP beyond platform behavior, and
  no revocation checking in `--ca-file` mode.
- ECDSA, RSA-PSS, multi-signer CMS. ECDSA/EdDSA certificate chains in
  `--ca-file` mode are reported as untrusted with an unsupported-chain-algorithm
  reason, not as a signature failure.
- Accepting `mcpb` 2.1.2 or current upstream `main` signed layouts.
- Changing what `pack` writes.

## 12. Known limitation

Claude Desktop's own verification path currently never returns `signed`
(upstream issue #305), so a correctly signed bundle may still be treated as
unsigned by that client until it updates. The acceptance target is: spec-correct
ZIP, cryptographically verifiable signature, independent verification with
OpenSSL and Windows tooling, and installability of the signed ZIP when the
signature policy is off. This limitation is external and changes neither the
format nor the implementation.
