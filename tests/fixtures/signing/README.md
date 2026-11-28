# Test identities for signing

`make.sh` regenerates every certificate and key here; the certificates are
checked in so the tests need neither `openssl` nor a key generation round.
`upstream.sh` generates the two bundles the upstream CLIs produce
(`upstream-212.mcpb`, `upstream-main.mcpb`), which the tests use to check what
this crate says about layouts it does not write.

They are test data, not secrets: the private keys are published in this
repository on purpose, and nothing here may sign anything that matters.

| file | what it is |
| --- | --- |
| `ca.pem`, `ca.key` | an RSA-2048 code-signing CA, valid 2000-2100 |
| `intermediate.pem`, `intermediate.key` | a CA signed by `ca`, for chain tests |
| `leaf.pem`, `leaf.key` | an RSA-2048 signer with the codeSigning EKU, signed by `ca` |
| `chained.pem`, `chained.key` | the same, signed by `intermediate` |
| `expired.pem`, `expired.key` | an RSA-2048 signer that expired in 2020 |
| `self-signed.pem`, `self-signed.key` | an RSA-2048 signer that is its own issuer |
| `no-eku.pem`, `no-eku.key` | an RSA-2048 signer without a codeSigning EKU |
| `weak.pem`, `weak.key` | an RSA-1024 signer, below what code signing needs |
| `ec-ca.pem`, `ec-ca.key` | an ECDSA P-256 CA, whose signatures the `--ca-file`
  check does not implement |
| `ec-signed.pem`, `ec-signed.key` | an RSA-2048 signer signed by `ec-ca`, so
  that the chain has one link the pure-Rust check cannot verify |
| `other.key` | an RSA-2048 key that belongs to no certificate here |
| `encrypted.key` | `leaf.key` as encrypted PKCS#8, passphrase `mcpb-test` |
| `pkcs1.key` | `leaf.key` as PKCS#1 `RSA PRIVATE KEY` |
