#!/bin/sh
# Regenerates the two bundles the upstream CLIs produce, so that the tests can
# check what this crate says about layouts it does not write.
#
# Needs `openssl` and `python3`, which is why the results are committed.
set -eu
cd "$(dirname "$0")"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

python3 - "$work/bundle.mcpb" <<'EOF'
import sys, zipfile

manifest = (
    '{"manifest_version": "0.3", "name": "upstream-fixture",'
    ' "version": "1.0.0", "description": "A bundle signed by the upstream CLI.",'
    ' "author": {"name": "mcpb"},'
    ' "server": {"type": "binary", "entry_point": "server/fixture",'
    ' "mcp_config": {"command": "${__dirname}/server/fixture", "args": []}},'
    ' "compatibility": {"platforms": ["linux"]}}'
)
with zipfile.ZipFile(sys.argv[1], "w", zipfile.ZIP_DEFLATED) as archive:
    archive.writestr("manifest.json", manifest)
    archive.writestr("server/fixture", "\x7fELF fake")
EOF

openssl cms -sign -binary -in "$work/bundle.mcpb" -signer leaf.pem -inkey leaf.key \
  -outform DER -out "$work/signature.der" -md sha256

# 2.1.2: the block is appended and the zip's comment length is left alone, so the
# block trails a comment that ends where it starts.
#
# main: the block is appended and the comment length is bumped afterwards, so the
# signature covers bytes that differ from the delivered ones.
for layout in 212 main; do
  python3 - "$work/bundle.mcpb" "$work/signature.der" "$layout" "upstream-$layout.mcpb" <<'EOF'
import struct, sys

zip_bytes = open(sys.argv[1], "rb").read()
der = open(sys.argv[2], "rb").read()
layout = sys.argv[3]
out = sys.argv[4]

with open(out, "wb") as bundle:
    block = b"MCPB_SIG_V1" + struct.pack("<I", len(der)) + der + b"MCPB_SIG_END"
    written = bytearray(zip_bytes)
    if layout == "main":
        end = written.rfind(b"PK\x05\x06")
        comment = struct.unpack_from("<H", written, end + 20)[0]
        struct.pack_into("<H", written, end + 20, comment + len(block))
    bundle.write(written + block)
EOF
done
