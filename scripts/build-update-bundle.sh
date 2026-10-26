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

# Build an unsigned app-update bundle for photo-frame-update.
#
#   manifest.json      release metadata plus a SHA-256 for every file
#   bin/photo-frame-manager
#   bin/photo-frame-display
#   bin/photo-frame-update
#
# Output, in the output directory:
#
#   photo-frame-<version>-<arch>.tar.gz
#   photo-frame-<version>-<arch>.tar.gz.sha256   (sha256sum format)
#
# This script does not sign anything and holds no key material. Sign the
# .sha256 file with scripts/sign-checksum.sh on a machine that holds the
# Ed25519 private key; see docs/updates.md.

set -euo pipefail

VERSION="${VERSION:-}"
SEQUENCE="${SEQUENCE:-}"
ARCH="${ARCH:-arm64}"
OUT_DIR="${OUT_DIR:-dist/update}"
MIN_VERSION="${MIN_VERSION:-}"
MANAGER="${MANAGER:-target/release/photo-frame-manager}"
DISPLAY="${DISPLAY:-c/photo-frame-display}"
UPDATE="${UPDATE:-target/release/photo-frame-update}"

usage() {
  cat <<'EOF'
Usage: scripts/build-update-bundle.sh --version X.Y.Z --sequence N [options]

Required (flag or environment variable):
  --version VERSION      Release version, also the release directory name.
  --sequence N           Monotonic anti-rollback sequence (use 1+ for bundles).

Options:
  --arch ARCH            Target architecture (default: arm64).
  --min-version VERSION  Oldest installed version the bundle supports.
  --out DIR              Output directory (default: dist/update).
  --manager PATH         Manager binary (default: target/release/photo-frame-manager).
  --display PATH         Display binary (default: c/photo-frame-display).
  --update PATH          Updater binary (default: target/release/photo-frame-update).

Environment:
  SOURCE_DATE_EPOCH      Pin file and archive mtimes for a reproducible bundle. Defaults to
                         the last git commit time when the tree is a git checkout.
EOF
}

die() {
  echo "error: $*" >&2
  exit 1
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)
      [ "$#" -ge 2 ] || die "--version requires a value"
      VERSION="$2"
      shift 2
      ;;
    --sequence)
      [ "$#" -ge 2 ] || die "--sequence requires a value"
      SEQUENCE="$2"
      shift 2
      ;;
    --arch)
      [ "$#" -ge 2 ] || die "--arch requires a value"
      ARCH="$2"
      shift 2
      ;;
    --min-version)
      [ "$#" -ge 2 ] || die "--min-version requires a value"
      MIN_VERSION="$2"
      shift 2
      ;;
    --out)
      [ "$#" -ge 2 ] || die "--out requires a value"
      OUT_DIR="$2"
      shift 2
      ;;
    --manager)
      [ "$#" -ge 2 ] || die "--manager requires a value"
      MANAGER="$2"
      shift 2
      ;;
    --display)
      [ "$#" -ge 2 ] || die "--display requires a value"
      DISPLAY="$2"
      shift 2
      ;;
    --update)
      [ "$#" -ge 2 ] || die "--update requires a value"
      UPDATE="$2"
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument '$1' (try --help)"
      ;;
  esac
done

[ -n "$VERSION" ] || die "--version is required (or set VERSION)"
[ -n "$SEQUENCE" ] || die "--sequence is required (or set SEQUENCE)"

# Must match is_safe_version() in src/update/mod.rs: the version becomes a
# release directory name, so reject anything that could escape releases/.
case "$VERSION" in
  .* | *[!A-Za-z0-9._+-]* | "") die "version '$VERSION' must match [A-Za-z0-9._+-]+ and not start with '.'" ;;
esac
[ "${#VERSION}" -le 64 ] || die "version is longer than 64 characters"

case "$SEQUENCE" in
  *[!0-9]* | "") die "sequence '$SEQUENCE' must be a non-negative integer" ;;
esac
if [ "$SEQUENCE" = "0" ]; then
  echo "warning: sequence 0 is reserved for the release seeded by the .deb; use 1 or higher for published bundles" >&2
fi

case "$ARCH" in
  arm64 | aarch64) ;;
  *) die "arch '$ARCH' is not supported by photo-frame-update (arm64 or aarch64)" ;;
esac

if [ -n "$MIN_VERSION" ]; then
  case "$MIN_VERSION" in
    .* | *[!A-Za-z0-9._+-]* | "") die "min-version '$MIN_VERSION' must match [A-Za-z0-9._+-]+ and not start with '.'" ;;
  esac
  [ "${#MIN_VERSION}" -le 64 ] || die "min-version is longer than 64 characters"
fi

for binary in "$MANAGER" "$DISPLAY" "$UPDATE"; do
  [ -f "$binary" ] || die "missing binary '$binary' (build it first: make c rust)"
  [ -x "$binary" ] || die "'$binary' is not executable"
done

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# Stable mtime for a reproducible archive: SOURCE_DATE_EPOCH if set, else the
# commit time of the tree being built.
epoch="${SOURCE_DATE_EPOCH:-}"
if [ -z "$epoch" ]; then
  epoch="$(git log -1 --format=%ct 2>/dev/null || true)"
fi
[ -n "$epoch" ] || epoch=0

# BSD date takes -r <seconds>; GNU date takes -d @<seconds>.
touch_time="$(date -u -d "@${epoch}" +%Y%m%d%H%M.%S 2>/dev/null ||
  date -u -r "${epoch}" +%Y%m%d%H%M.%S 2>/dev/null || true)"

mkdir -p "$OUT_DIR"
OUT_DIR="$(cd "$OUT_DIR" && pwd)"
BUNDLE_NAME="photo-frame-${VERSION}-${ARCH}.tar.gz"

stage="$(mktemp -d "${TMPDIR:-/tmp}/photo-frame-bundle.XXXXXX")"
cleanup() {
  rm -rf "$stage"
}
trap cleanup EXIT

mkdir -p "$stage/bin"
install -m 0755 "$MANAGER" "$stage/bin/photo-frame-manager"
install -m 0755 "$DISPLAY" "$stage/bin/photo-frame-display"
install -m 0755 "$UPDATE" "$stage/bin/photo-frame-update"

{
  printf '{\n'
  printf '  "format": 1,\n'
  printf '  "version": "%s",\n' "$VERSION"
  printf '  "sequence": %s,\n' "$SEQUENCE"
  printf '  "arch": "%s",\n' "$ARCH"
  if [ -n "$MIN_VERSION" ]; then
    printf '  "min_version": "%s",\n' "$MIN_VERSION"
  else
    printf '  "min_version": null,\n'
  fi
  printf '  "files": [\n'
  first=1
  for name in photo-frame-manager photo-frame-display photo-frame-update; do
    if [ "$first" -eq 1 ]; then
      first=0
    else
      printf ',\n'
    fi
    printf '    {"path": "bin/%s", "sha256": "%s"}' \
      "$name" "$(sha256_of "$stage/bin/$name")"
  done
  printf '\n  ]\n'
  printf '}\n'
} >"$stage/manifest.json"
chmod 0644 "$stage/manifest.json"

if [ -n "$touch_time" ]; then
  find "$stage" -exec touch -t "$touch_time" {} +
fi

tar_args=()
if tar --version 2>/dev/null | head -n1 | grep -q 'GNU tar'; then
  # Sorted entries, no owner names or ids, fixed mtime: same inputs, same bytes.
  tar_args+=(
    --sort=name
    --owner=0
    --group=0
    --numeric-owner
    --mtime="@${epoch}"
  )
else
  # bsdtar (macOS): keep the archive buildable, with root ownership only and a
  # fixed entry order.
  tar_args+=(--uid 0 --gid 0 --uname root --gname root)
fi
tar_args+=(-cf - -C "$stage" manifest.json bin)

# `gzip -n` writes no filename or timestamp in the header, so the compressed
# bytes depend only on the tar input.
tar "${tar_args[@]}" | gzip -n -9 >"${OUT_DIR}/${BUNDLE_NAME}.part"
mv -f "${OUT_DIR}/${BUNDLE_NAME}.part" "${OUT_DIR}/${BUNDLE_NAME}"

if command -v sha256sum >/dev/null 2>&1; then
  checksum_cmd=(sha256sum)
else
  checksum_cmd=(shasum -a 256)
fi
(
  cd "$OUT_DIR"
  "${checksum_cmd[@]}" "$BUNDLE_NAME" >"${BUNDLE_NAME}.sha256"
)

echo "bundle:   ${OUT_DIR}/${BUNDLE_NAME}"
echo "checksum: ${OUT_DIR}/${BUNDLE_NAME}.sha256"
cat "${OUT_DIR}/${BUNDLE_NAME}.sha256"
echo
echo "Unsigned. Sign the checksum with the offline update key:"
echo "  UPDATE_SIGNING_KEY=... UPDATE_SIGNING_KEY_ID=... \\"
echo "    scripts/sign-checksum.sh ${OUT_DIR}/${BUNDLE_NAME}.sha256 ${OUT_DIR}/${BUNDLE_NAME}.sha256.sig"
