#!/usr/bin/env bash
# Photo Frame Manager — DRM/GBM/EGL digital photo frame.
# Copyright (C) 2026 Daniel Mikusa <dan@mikusa.com>
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU Affero General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
# GNU Affero General Public License for more details.
#
# You should have received a copy of the GNU Affero General Public License
# along with this program. If not, see <https://www.gnu.org/licenses/>.

# Sign a bundle's `.sha256` file with the offline Ed25519 update key.
#
#   UPDATE_SIGNING_KEY="$(cat update-key.pem)" UPDATE_SIGNING_KEY_ID=2026-10 \
#     scripts/sign-checksum.sh photo-frame-0.2.0-arm64.tar.gz.sha256 \
#     photo-frame-0.2.0-arm64.tar.gz.sha256.sig
#
# Produces the JSON envelope photo-frame-update expects:
#
#   {"key_id":"2026-10","signature":"<base64url of the raw 64-byte signature>"}
#
# The signature covers the exact bytes of the checksum file, which in turn
# covers the bundle. There are no certificates and no online lookups. The key
# is read from the environment only: it is never written to the repo, the
# device, or the release. The only public key that matters is the one pinned in
# src/update/keys.rs, which the owner adds before building a release. See
# docs/updates.md for key setup and rotation.

set -euo pipefail

die() {
  echo "error: $*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: scripts/sign-checksum.sh <checksum-file> [signature-file]

Signs the exact bytes of <checksum-file> with the Ed25519 update key and writes
the JSON envelope to [signature-file] (default: <checksum-file>.sig, mode 0600).

Required environment:
  UPDATE_SIGNING_KEY     PEM-encoded Ed25519 private key. Used in memory only.
  UPDATE_SIGNING_KEY_ID  Short key id recorded in the envelope, e.g. 2026-10.
EOF
}

case "${1:-}" in
  -h | --help)
    usage
    exit 0
    ;;
esac

[ "$#" -ge 1 ] && [ "$#" -le 2 ] || {
  usage >&2
  die "expected 1 or 2 arguments"
}

CHECKSUM_FILE="$1"
SIG_FILE="${2:-${CHECKSUM_FILE}.sig}"

[ -f "$CHECKSUM_FILE" ] || die "checksum file '$CHECKSUM_FILE' does not exist"
[ -r "$CHECKSUM_FILE" ] || die "checksum file '$CHECKSUM_FILE' is not readable"

# Fail closed: no key material, no signature.
[ -n "${UPDATE_SIGNING_KEY:-}" ] || die "UPDATE_SIGNING_KEY is not set; refusing to sign"
[ -n "${UPDATE_SIGNING_KEY_ID:-}" ] || die "UPDATE_SIGNING_KEY_ID is not set; refusing to sign"

# The key id is embedded verbatim in JSON, so keep it to a safe alphabet. It is
# also what the device matches against its pinned key list.
case "$UPDATE_SIGNING_KEY_ID" in
  *[!A-Za-z0-9._-]* | "") die "UPDATE_SIGNING_KEY_ID may only contain [A-Za-z0-9._-]" ;;
esac

command -v openssl >/dev/null 2>&1 || die "openssl is required"

umask 077
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/photo-frame-sign.XXXXXX")"
cleanup() {
  rm -rf "$work_dir"
}
trap cleanup EXIT

key_file="${work_dir}/update-key.pem"
signature_file="${work_dir}/signature.bin"

# printf adds the trailing newline PEM needs; a key passed without one still
# parses.
printf '%s\n' "$UPDATE_SIGNING_KEY" >"$key_file"
chmod 0600 "$key_file"

# `-rawin` exists from OpenSSL 3.0 and is what signs the message bytes directly
# for Ed25519. Older versions cannot do this, so fail with a clear message
# instead of producing a signature that will not verify.
openssl pkeyutl -help 2>&1 | grep -q -- '-rawin' ||
  die "openssl is too old: pkeyutl -rawin requires OpenSSL 3.0 or newer"

# Check the key is Ed25519 (the only algorithm photo-frame-update verifies) by
# looking for its PKCS#8 algorithm OID (1.3.101.112) in the DER encoding. This
# never prints any key material.
if ! openssl pkey -in "$key_file" -outform DER 2>/dev/null |
  od -An -tx1 | tr -d ' \n' | grep -q '06032b6570'; then
  die "UPDATE_SIGNING_KEY is not a readable Ed25519 private key"
fi

openssl pkeyutl -sign -rawin -inkey "$key_file" -in "$CHECKSUM_FILE" -out "$signature_file" ||
  die "failed to sign '$CHECKSUM_FILE'"

[ -s "$signature_file" ] || die "openssl produced an empty signature"
# Ed25519 signatures are always 64 bytes. Anything else is not what the device
# verifies, so refuse to publish it.
signature_size="$(wc -c <"$signature_file" | tr -d ' ')"
[ "$signature_size" = "64" ] ||
  die "expected a 64-byte Ed25519 signature, got ${signature_size} bytes"

# base64url without padding, as photo-frame-update decodes.
signature_b64="$(openssl base64 -A -in "$signature_file" | tr '+/' '-_' | tr -d '=')"

cat >"${work_dir}/envelope.json" <<EOF
{"key_id":"${UPDATE_SIGNING_KEY_ID}","signature":"${signature_b64}"}
EOF

# Land the envelope atomically with 0600 permissions.
install -m 0600 "${work_dir}/envelope.json" "$SIG_FILE"
echo "signed ${CHECKSUM_FILE} -> ${SIG_FILE} (key ${UPDATE_SIGNING_KEY_ID})"
