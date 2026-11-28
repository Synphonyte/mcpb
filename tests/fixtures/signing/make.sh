#!/bin/sh
# Regenerates the test identities in this directory. They are test data: the
# private keys are published on purpose.
set -eu
cd "$(dirname "$0")"

from=20000101000000Z
to=21000101000000Z
expired_to=20200101000000Z

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Every certificate needs a serial of its own: a CMS signature names its signer
# by issuer and serial, so two certificates sharing both would be ambiguous.
serial=4096

# sign <name> <subject> <extfile> <issuer or self> <issuer key> <from> <to>
sign() {
  name=$1 subject=$2 extfile=$3 issuer=$4 issuer_key=$5 start=$6 end=$7
  openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "$name.key" 2>/dev/null
  openssl req -new -key "$name.key" -subj "$subject" -out "$work/$name.csr"
  if [ "$issuer" = self ]; then
    openssl x509 -req -in "$work/$name.csr" -signkey "$name.key" -sha256 \
      -not_before "$start" -not_after "$end" -extfile "$extfile" -out "$name.pem"
  else
    serial=$((serial + 1))
    printf "%x\n" "$serial" > "$work/$name.srl"
    openssl x509 -req -in "$work/$name.csr" -CA "$issuer" -CAkey "$issuer_key" \
      -CAserial "$work/$name.srl" -sha256 -not_before "$start" -not_after "$end" \
      -extfile "$extfile" -out "$name.pem"
  fi
}

cat > "$work/signer.ext" <<'EOF'
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature
extendedKeyUsage=codeSigning
EOF

cat > "$work/plain.ext" <<'EOF'
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature
EOF

cat > "$work/ca.ext" <<'EOF'
basicConstraints=critical,CA:TRUE
keyUsage=critical,keyCertSign,cRLSign
EOF

# A code-signing CA, and an intermediate it signed.
sign ca "/CN=mcpb test CA" "$work/ca.ext" self "" "$from" "$to"
sign intermediate "/CN=mcpb test intermediate" "$work/ca.ext" ca.pem ca.key "$from" "$to"

sign leaf "/CN=mcpb test signer" "$work/signer.ext" ca.pem ca.key "$from" "$to"
sign chained "/CN=mcpb test chained signer" "$work/signer.ext" intermediate.pem intermediate.key "$from" "$to"
sign expired "/CN=mcpb test expired signer" "$work/signer.ext" ca.pem ca.key "$from" "$expired_to"
sign self-signed "/CN=mcpb test self-signed" "$work/signer.ext" self "" "$from" "$to"
sign no-eku "/CN=mcpb test no eku" "$work/plain.ext" ca.pem ca.key "$from" "$to"

# An RSA key below the 2048-bit floor.
weak() {
  openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:1024 -out weak.key 2>/dev/null
  openssl req -new -key weak.key -subj "/CN=mcpb test weak" -out "$work/weak.csr"
  openssl x509 -req -in "$work/weak.csr" -signkey weak.key -sha256 \
    -not_before "$from" -not_after "$to" -extfile "$work/signer.ext" -out weak.pem
}
weak

# An ECDSA CA, so that one link of the chain is a signature `--ca-file` does not
# implement. The signer's own key is still RSA: only the issuer signs with ECDSA.
ec() {
  openssl ecparam -name prime256v1 -genkey -noout -out ec-ca.key
  openssl req -new -key ec-ca.key -subj "/CN=mcpb test EC CA" -out "$work/ec.csr"
  openssl x509 -req -in "$work/ec.csr" -signkey ec-ca.key -sha256 \
    -not_before "$from" -not_after "$to" -extfile "$work/ca.ext" -out ec-ca.pem
}
ec
sign ec-signed "/CN=mcpb test EC-signed signer" "$work/signer.ext" ec-ca.pem ec-ca.key "$from" "$to"

# A key that belongs to no certificate above.
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out other.key 2>/dev/null

# The leaf key in the other shapes a caller may have.
openssl pkcs8 -topk8 -in leaf.key -passout pass:mcpb-test -out encrypted.key
openssl rsa -in leaf.key -traditional -out pkcs1.key 2>/dev/null
