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

# Convert an OpenSSL Ed25519 private key to an OpenSSH public-key line.
#
#   scripts/openssh-public-key.sh update-signing-2026-10.pem 2026-10 \
#     > packaging/update-keys/2026-10.pub
#
# The private key stays PEM because that is what scripts/sign-checksum.sh uses
# with OpenSSL. The public file is the familiar ssh-ed25519 form that build.rs
# parses. The second argument is only the comment on the public key line.

set -euo pipefail

die() {
  echo "error: $*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: scripts/openssh-public-key.sh <private-key.pem> [comment]

Prints one OpenSSH public-key line:
  ssh-ed25519 <base64> <comment>

The private key must be a PEM Ed25519 private key generated with:
  openssl genpkey -algorithm ED25519 -out <private-key.pem>
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
  die "expected one or two arguments"
}

PRIVATE_KEY="$1"
COMMENT="${2:-photo-frame-update}"

[ -f "$PRIVATE_KEY" ] || die "private key '$PRIVATE_KEY' does not exist"
[ -r "$PRIVATE_KEY" ] || die "private key '$PRIVATE_KEY' is not readable"
command -v openssl >/dev/null 2>&1 || die "openssl is required"
command -v base64 >/dev/null 2>&1 || die "base64 is required"

# Check the key is Ed25519 by looking for its PKCS#8 algorithm OID
# (1.3.101.112) in the DER encoding. This prints no key material.
if ! openssl pkey -in "$PRIVATE_KEY" -outform DER 2>/dev/null |
  od -An -tx1 | tr -d ' \n' | grep -q '06032b6570'; then
  die "'$PRIVATE_KEY' is not a readable Ed25519 private key"
fi

raw_size="$(openssl pkey -in "$PRIVATE_KEY" -pubout -outform DER |
  tail -c 32 | wc -c | tr -d ' ')"
[ "$raw_size" = "32" ] ||
  die "expected a 32-byte Ed25519 public key, got ${raw_size} bytes"

# OpenSSH wire format: string "ssh-ed25519" + string <32-byte key>.
public_b64="$(
  {
    printf '\x00\x00\x00\x0bssh-ed25519\x00\x00\x00\x20'
    openssl pkey -in "$PRIVATE_KEY" -pubout -outform DER | tail -c 32
  } | base64 | tr -d '\n'
)"

[ -n "$public_b64" ] || die "failed to encode the public key"
printf 'ssh-ed25519 %s %s\n' "$public_b64" "$COMMENT"
