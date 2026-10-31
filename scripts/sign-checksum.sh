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

# Sign a bundle's `.sha256` file with an OpenSSH Ed25519 key.
#
#   UPDATE_SIGNING_KEY="$(cat update-signing-2026-10)" \
#     scripts/sign-checksum.sh photo-frame-0.2.0-arm64.tar.gz.sha256
#
# The output is the standard armored OpenSSH signature format produced by
# `ssh-keygen -Y sign -n photo-frame-update`. photo-frame-update verifies it
# in-process against the public keys in packaging/update-keys/*.pub. There is no
# certificate, expiry, key-id field, or allowed_signers file at install time.

set -euo pipefail

die() {
  echo "error: $*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: scripts/sign-checksum.sh <checksum-file> [signature-file]

Signs the exact bytes of <checksum-file> with an OpenSSH Ed25519 key and writes
the armored signature to [signature-file] (default: <checksum-file>.sig, mode
0600).

Required environment:
  UPDATE_SIGNING_KEY     OpenSSH Ed25519 private key. Used in memory only.

The key must be unencrypted for non-interactive CI use, or available through
ssh-agent. The public key is stored separately in packaging/update-keys/.
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

command -v ssh-keygen >/dev/null 2>&1 || die "ssh-keygen is required"

umask 077
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/photo-frame-sign.XXXXXX")"
cleanup() {
  rm -rf "$work_dir"
}
trap cleanup EXIT

key_file="${work_dir}/update-key"
printf '%s\n' "$UPDATE_SIGNING_KEY" >"$key_file"
chmod 0600 "$key_file"

# This signer is for the OpenSSH key format. A PKCS#8 PEM key would be silently
# unsupported by ssh-keygen, so reject it with a useful message.
if ! grep -q -- '-----BEGIN OPENSSH PRIVATE KEY-----' "$key_file"; then
  die "UPDATE_SIGNING_KEY must be an OpenSSH private key (BEGIN OPENSSH PRIVATE KEY)"
fi

# Fail before generating a partial signature if ssh-keygen would prompt for a
# passphrase. CI has no terminal; use an unencrypted key or ssh-agent.
if ! ssh-keygen -y -f "$key_file" </dev/null >/dev/null 2>&1; then
  die "the OpenSSH key is unreadable or requires a passphrase; CI needs an unencrypted key or ssh-agent"
fi

cp "$CHECKSUM_FILE" "${work_dir}/checksum"
(
  cd "$work_dir"
  ssh-keygen -Y sign -f "$key_file" -n photo-frame-update \
    -O hashalg=sha256 checksum
)

signature_file="${work_dir}/checksum.sig"
[ -s "$signature_file" ] || die "ssh-keygen produced an empty signature"
grep -q -- '-----BEGIN SSH SIGNATURE-----' "$signature_file" ||
  die "ssh-keygen did not produce an SSH signature"

# Land the signature atomically with 0600 permissions.
install -m 0600 "$signature_file" "$SIG_FILE"
echo "signed ${CHECKSUM_FILE} -> ${SIG_FILE} (SSH key)"
