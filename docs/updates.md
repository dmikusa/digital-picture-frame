# Updates

This document describes the update tool that is in the tree today:
`photo-frame-update`, the signed bundle it installs, the state it keeps, and the
manual CI workflow that signs a release. It covers the app-only update path:
`photo-frame-manager`, `photo-frame-display`, and the updater itself. The
operating system, kernel, firmware, and bootloader are not part of a bundle;
they follow the apt policy in
[OS security updates](#10-os-security-updates-and-kernel-cadence).

The `.deb` seeds the first release at sequence 0. Later releases are signed
bundles installed by `photo-frame-update`. There are no A/B partitions, no RAUC
or SWUpdate, and no `dietpi-backup`.

## 1. Overview and trust model

A release is three files: a tarball, a checksum file, and a signature file. The
tarball holds `manifest.json` and the three binaries. The checksum file holds the
SHA-256 of the tarball. The signature file holds an Ed25519 signature over the
exact bytes of the checksum file.

The device trusts a fixed list of Ed25519 public keys compiled into
`photo-frame-update` (`TRUSTED_KEYS` in `src/update/keys.rs`). Each key has a
short ID such as `2026-10`. The signature envelope names the key ID; the updater
looks that ID up in the compiled list, verifies the signature against the raw
32-byte public key, and refuses the bundle if the ID is unknown.

The compiled list starts empty on purpose. A build with no pinned key refuses
every install. Adding a key and rebuilding the tool is a required provisioning
step; it is covered in section 5.

There is no certificate, no certificate authority, no expiry date, no revocation
list, and no network lookup during verification. A signature that was valid in
2026 is still valid in 2036. Everything needed to check a release is either in
the bundle or in the updater binary.

The trust anchor is software only. The Pi Zero 2 W has no secure boot, so
somebody with the SD card can replace `photo-frame-update` and its compiled key
list as easily as they can replace the manager. Signing removes the "malicious
network or mirror" and "wrong file" risks. It does not remove the physical-access
risk.

```
local file or HTTPS ──> bundle.tar.gz ──> .sha256 ──> Ed25519 .sha256.sig
                              │              │              │
                              └──────────────┴──────────────┘
                                             │
                               photo-frame-update (compiled keys)
                                             │
                          verify, stage, extract, flip, health check
```

## 2. Bundle layout

`scripts/build-update-bundle.sh` writes two files, for example:

```
photo-frame-0.2.0-arm64.tar.gz
photo-frame-0.2.0-arm64.tar.gz.sha256
```

Signing adds the third:

```
photo-frame-0.2.0-arm64.tar.gz.sha256.sig
```

The name is `photo-frame-<version>-<arch>.tar.gz`. The sequence is **not** in
the filename; it lives in `manifest.json` and in the state file. The version is
also the release directory name, so it must match `[A-Za-z0-9._+-]+`, must not
start with a dot, and is capped at 64 characters.

The tarball contains a manifest and a `bin/` directory:

```
manifest.json
bin/photo-frame-manager
bin/photo-frame-display
bin/photo-frame-update
```

All three binaries are required. The updater refuses a bundle whose manifest does
not list them.

`manifest.json`:

```json
{
  "format": 1,
  "version": "0.2.0",
  "sequence": 7,
  "arch": "arm64",
  "min_version": "0.1.0",
  "files": [
    {"path": "bin/photo-frame-manager", "sha256": "…"},
    {"path": "bin/photo-frame-display", "sha256": "…"},
    {"path": "bin/photo-frame-update", "sha256": "…"}
  ]
}
```

| Field | Meaning |
|-------|---------|
| `format` | Bundle format. Only `1` is accepted. |
| `version` | Version string and release directory name. |
| `sequence` | Monotonic anti-rollback floor. |
| `arch` | Target architecture. Only `arm64` and `aarch64` are accepted. |
| `min_version` | Optional. Refuse if the installed `current` is older than this. `null` means no minimum. |
| `files[]` | Every installed file with its relative path and SHA-256. |

There is no `product`, `size`, or `mode` field. The extractor sets modes: files
under `bin/` become `0755`, everything else becomes `0644`.

The `.sha256` file is standard `sha256sum` output. Note the two spaces between
the digest and the file name:

```
9c1f0b6d…  photo-frame-0.2.0-arm64.tar.gz
```

The updater accepts exactly one non-empty line, requires the filename column, and
ignores the recorded filename when locating the bundle.

The `.sha256.sig` file is a small JSON envelope holding the raw signature:

```json
{"key_id":"2026-10","signature":"iV7K…"}
```

`signature` is the base64url encoding of 64 raw Ed25519 signature bytes, with or
without padding. It signs the exact bytes of the `.sha256` file, not the tarball.
The chain is deliberate: the signature authenticates the checksum, and the
checksum authenticates the tarball, so all three files are required.

Extraction and parsing have hard caps and an allowlist:

| Limit | Value |
|-------|-------|
| Bundle size | 100 MiB |
| Total extracted size | 512 MiB |
| Archive entries | 10,000 |
| `manifest.json` size | 1 MiB |
| Signature envelope | 64 KiB |
| Checksum sidecar | 8 KiB |

Only regular files and directories are extracted. Absolute paths, `..`,
symlinks, hardlinks, device nodes, and unknown entry types are rejected. Nothing
is written to `releases/` until the signature and hashes pass.

## 3. Commands

```
photo-frame-update install <path|https-url> [--sha256 <hex>]
photo-frame-update rollback
photo-frame-update list
photo-frame-update status
photo-frame-update gc
photo-frame-update guard [--grace-seconds N]
photo-frame-update keys
```

`keys` is the only command that works without root; it only reads the compiled
key list. The others read root-only state under `/var/lib/photo-frame/update` or
change the install, so run them with `sudo`.

Exit codes are `0` on success, `1` on an install or verification failure, and `2`
on a usage error.

### Local install

Copy all three files into one directory. The sidecars must sit next to the
tarball; the updater does not look anywhere else.

```bash
sudo photo-frame-update install \
  /media/usb0/photo-frame-0.2.0-arm64.tar.gz
```

`--sha256` is optional for a local file. When supplied, the copied bundle must
match it as well as the `.sha256` sidecar. Verification runs against the copy in
staging, so the file cannot be swapped between hashing and extraction.

### HTTPS install

```bash
sudo photo-frame-update install \
  https://github.com/dmikusa/photo-frame/releases/download/v0.2.0/photo-frame-0.2.0-arm64.tar.gz \
  --sha256 9c1f0b6d…
```

`--sha256` is required for a URL. The updater fetches the checksum and signature
from the same URL with `.sha256` and `.sha256.sig` appended, verifies the
signature, downloads the bundle, and checks it against both the sidecar and the
digest you passed. The out-of-band digest catches "this is a validly signed
release, but not the one I meant to install".

Only `https://` URLs are accepted. A redirect to anything other than HTTPS is
refused, and more than five redirects is an error. Connect timeout is 30 seconds
and the whole request times out at 120 seconds.

### Inspecting and managing releases

```bash
$ sudo photo-frame-update list
VERSION                SIZE  STATE
0.2.0               12.3 MiB  current
0.1.0               12.1 MiB  previous
0.1.0-rc1           12.0 MiB
```

`list` shows resident releases, newest first, with a recursive size and the
`current`/`previous` flags from state.

```bash
$ sudo photo-frame-update status
current:    0.2.0
previous:   0.1.0
last_good:  0.2.0
pending:    none
sequence:   7
releases:   3
current link: /opt/photo-frame/current -> releases/0.2.0
```

`sequence` is the anti-rollback floor, not the sequence of the active release, so
it can be higher than the current version after a rollback. If the symlink points
at a missing directory, `status` marks it `(MISSING)`.

```bash
$ sudo photo-frame-update gc
removed: 0.0.9
kept:    3 releases
```

`gc` keeps `current`, `previous`, and the four most recently modified release
directories, so at most six remain. It runs automatically after a successful
install. A `gc` failure is a warning, not an install failure.

```bash
$ sudo photo-frame-update rollback
rolled back from 0.2.0 to 0.1.0
```

`rollback` takes no arguments. It activates the recorded `previous` release,
runs the same health check as an install, and swaps `current` and `previous` on
success. It does not lower the sequence floor. It refuses when no previous
release is recorded, when the directory is missing, or when the previous release
is already current.

```bash
$ sudo photo-frame-update guard
no pending update
```

`guard` is the boot-guard entry point. It reports `no pending update`, confirms
an active pending release, rolls back an unhealthy one, or clears a pending
marker whose release never became active. `--grace-seconds N` sleeps before the
health check; the systemd unit passes 30. The default is 0.

```bash
$ photo-frame-update keys
2026-10
```

`keys` prints one key ID per line. It does not print fingerprints. If the build
has no trusted keys, it prints a warning to stderr and exits 0:

```
warning: no trusted update keys are baked into this build; all installs will be refused
```

### Development flags

`--root`, `--state-dir`, `--log`, and `--no-restart` exist for tests. They let a
developer point the tool at a temporary root and state directory and skip
systemctl and the health check. They are not part of the supported production
flow and are not covered by the install procedure.

## 4. Install, activation, health check, rollback, guard

An install never writes over a running binary. It fetches, verifies, extracts
into staging, then flips one symlink.

1. Load `/var/lib/photo-frame/update/state.json`. A missing file is an empty
   state; a corrupt one aborts the install.
2. Verify the signature over the `.sha256` bytes using the key named by
   `key_id`. An unknown key ID, an empty compiled key list, or a bad signature
   ends the install here.
3. Copy (local) or download (HTTPS) the bundle into
   `/opt/photo-frame/.staging` (mode 0700), then hash that copy.
4. Compare the hash with the sidecar and, when supplied, with `--sha256`.
5. Extract into a unique directory under `.staging`. Read and validate
   `manifest.json`: `format`, version name, `arch`, `min_version`, and every
   `files[]` hash. All three binaries must be listed and present.
6. Apply policy checks:
   - Refuse if `releases/<version>` already exists. The same version cannot be
     installed twice.
   - Refuse if `sequence` is lower than the stored floor. An equal sequence is
     accepted only when the target version directory does not already exist.
   - Refuse if `min_version` is set and the active `current` version is older.
7. Rename the extracted directory to `/opt/photo-frame/releases/<version>` and
   flush the releases directory.
8. Write the pending marker into `state.json` with `from`, `to`, `sequence`,
   `key_id`, `source`, and `started_at`.
9. Create a temporary symlink and rename it over `/opt/photo-frame/current`.
   That rename is the activation point.
10. Restart `photo-frame-display.service` and `photo-frame-manager.service`.
11. Run the health check for up to 30 seconds.
12. On success, set `current` to the new version, record the old version as
    `previous` (when different), set `last_good`, raise the sequence floor,
    clear `pending`, append to the audit log, and run GC.
13. On failure, activate the recorded previous release if it still exists and
    restart the services. Then clear `pending`, record the revert, and return an
    error. If there is no usable previous release, remove the `current` symlink,
    clear `pending`, record the failure, and return an error.

The health check probes every 500 ms and requires exactly two things:

| Check | Pass condition |
|-------|----------------|
| Services | `systemctl is-active --quiet` succeeds for both units |
| Sockets | `/run/photo-frame/photo-frame.sock` and `/run/photo-frame/control.sock` exist and are sockets |

There is no `healthy` file, no restart-count check, and no photos-directory
check. The socket paths are hardcoded, so moving the sockets in `config.toml` and
`display.env` makes a post-update health check fail.

### Boot guard

`photo-frame-update-guard.service` is a oneshot unit that runs **after** both app
units on every boot:

```ini
After=photo-frame-manager.service photo-frame-display.service
Wants=photo-frame-manager.service photo-frame-display.service
ExecStart=/opt/photo-frame/current/bin/photo-frame-update guard --grace-seconds 30
```

It is enabled by the `.deb` and is a no-op when `pending` is null. When an
active pending install exists it waits 30 seconds, runs the health check, and
then either confirms the release (updating `current`, `previous`, `last_good`,
and `sequence`) or reverts to `pending.from` and restarts the services.

Activation is detected from two sources. The guard treats the pending release as
active if either the `current` symlink or `state.current` names `pending.to`. The
symlink is what the services actually run, and `state.current` covers the window
where state was updated first. If neither names the pending release, the guard
assumes the install never activated, clears `pending`, and records `cleared`.

Because the unit runs after the app services, it cannot stop a broken release
from starting. It detects the failure during the grace period plus health check
and rolls back. There is no attempt counter and no `guard --clear`; the guard
confirms, reverts, or clears a marker for a release that never activated.

### Rollback

`rollback` is the manual path. It requires a recorded `previous` release and
writes a pending marker before flipping, so a power loss mid-rollback is handled
by the guard the same way as an install. On success it swaps `current` and
`previous` and leaves `last_good` on the now-active release. The sequence floor
does not change, so rolling back does not authorize an older bundle later.

## 5. Generating a signing key and pinning it

Keys are Ed25519. OpenSSL 3 is enough to generate one and sign a checksum; the
`sign-checksum.sh` script wraps the OpenSSL calls and adds validation.

Generate a key pair:

```bash
openssl genpkey -algorithm ED25519 -out update-signing-2026-10.pem
chmod 600 update-signing-2026-10.pem
```

The private key is a small PEM file. Keep it in a password manager or encrypted
volume, never in the repository and never on the frame. Back it up. A lost key
cannot sign anything the device will accept; recovery means a second trusted key
or a reflash.

Derive the raw public key. The last 32 bytes of the DER SubjectPublicKeyInfo are
the key itself:

```bash
openssl pkey -in update-signing-2026-10.pem -pubout -outform DER \
  | tail -c 32 | xxd -p -c 32
```

Add those bytes to the compiled list in `src/update/keys.rs`. The struct field is
`bytes`:

```rust
pub const TRUSTED_KEYS: &[TrustedKey] = &[
    TrustedKey {
        id: "2026-10",
        bytes: [
            0x9f, 0x2c, 0x1d, /* … 32 bytes total … */
        ],
    },
];
```

Rebuild `photo-frame-update` and include the rebuilt binary in the release
bundle. The trusted set lives in the binary, so adding a key is a code change
plus a rebuild, not a file drop. There is no public-key file installed by apt and
no key directory to edit.

The fingerprint to compare against the signing machine is the SHA-256 of the same
32 bytes:

```bash
openssl pkey -in update-signing-2026-10.pem -pubout -outform DER \
  | tail -c 32 | sha256sum
```

`photo-frame-update keys` does not print fingerprints; compare IDs only there,
and keep the fingerprint check on the signing machine.

Build an unsigned bundle:

```bash
make update-bundle VERSION=0.2.0 SEQUENCE=7
```

That runs `scripts/build-update-bundle.sh` and writes
`dist/update/photo-frame-0.2.0-arm64.tar.gz` plus its `.sha256`. Sequence 0 is
reserved for the release seeded by the `.deb`; published bundles should use 1 or
higher.

Sign the checksum:

```bash
UPDATE_SIGNING_KEY="$(cat update-signing-2026-10.pem)" \
UPDATE_SIGNING_KEY_ID=2026-10 \
  scripts/sign-checksum.sh \
    dist/update/photo-frame-0.2.0-arm64.tar.gz.sha256
```

The script reads the key from the environment only, checks that OpenSSL supports
`pkeyutl -rawin`, verifies the key is Ed25519 by its PKCS#8 OID, requires a
64-byte signature, and writes the JSON envelope with mode 0600. The output is
`photo-frame-0.2.0-arm64.tar.gz.sha256.sig`.

There is no separate verify-only command. The same verification path runs at the
start of `install`; to exercise a signature without touching the real install,
point the tool at a throwaway root and state directory with `--root`,
`--state-dir`, `--log`, and `--no-restart`.

## 6. Cutting, building, and signing a release

CI builds both the `.deb` packages and the arm64 update bundle, and creates or
updates the draft release. The only manual gates are triggering the version bump,
signing the checksums, and publishing the draft.

### Release checklist at a glance

1. Run **Actions → Bump version** and enter the next `X.Y.Z`.
2. Let the bump PR pass CI and auto-merge.
3. Wait for **Build and Release** to build the `.deb` files and arm64 bundle,
   and to create or update the draft release `vX.Y.Z`.
4. Run **Sign Release Checksums** for `vX.Y.Z` and approve the
   `release-signing` environment.
5. Review and test the draft, then publish it.

The rest of this section explains each step.

The release tag and `Cargo.toml` use `X.Y.Z`. Builds use
`X.Y.Z-<short-sha>`:

- The `.deb` package version is `X.Y.Z-<short-sha>`. It seeds a release
  directory named `X.Y.Z-<short-sha>-seed` at sequence 0. The `-seed` suffix
  keeps the packaged release distinct from an update bundle built from the same
  commit.
- The update bundle is the app-update path. It uses `X.Y.Z-<short-sha>` as its
  release version and `major * 1_000_000 + minor * 1_000 + patch` as its
  sequence, so version `0.2.0` is sequence `2000`. Every main commit with the
  same `X.Y.Z` has the same sequence but a different release directory.
- The first bundle install records the seeded `.deb` release as `previous`.

### Before the first release

1. Generate the update key and pin its public key in `src/update/keys.rs`
   (section 5). Rebuild so both the `.deb` and the bundle contain an updater
   that trusts it.
2. Configure the `release-signing` environment (see
   [Workflow behavior](#workflow-behavior)). Add the private key as an
   environment secret, not a repository secret:
   ```bash
   gh secret set UPDATE_SIGNING_KEY --env release-signing \
     < update-signing-2026-10.pem
   gh secret set UPDATE_SIGNING_KEY_ID --env release-signing --body 2026-10
   ```
   The environment must already exist. Until the public key is pinned, every
   `photo-frame-update install` fails closed.
3. Configure the repository settings used by the bump workflow:
   - `RENOVATE_TOKEN` secret with Contents and Pull requests write access.
   - Auto-merge enabled.
   - A `dependencies` label.

### 1. Trigger the version bump

Run **Actions → Bump version** with the next `X.Y.Z` version. The workflow:

- validates that the new version is semver and greater than the current one,
- updates `Cargo.toml` and syncs the root package entry in `Cargo.lock`,
- pushes `bump-version-X.Y.Z`,
- opens a PR labeled `dependencies`,
- enables squash auto-merge.

The PR runs the normal CI checks. Merging it puts the new version on `main` and
triggers `build.yml`. Do not hand-edit the version unless the bump workflow is
unavailable; the merge must still land on `main`.

### 2. Let CI build and open the draft release

On every push to `main`, `build.yml` first checks whether `vX.Y.Z` is already
published. If it is, the build is a no-op. Otherwise it:

- builds `amd64` and `arm64` `.deb` packages,
- builds the `arm64` update bundle and `.sha256` sidecar,
- creates the draft release `vX.Y.Z` if it does not exist, or replaces the
  assets on the existing draft,
- uploads the `.deb` files, bundle, and checksum to the draft.

The bundle assets are named:

```text
photo-frame-<X.Y.Z>-<short-sha>-arm64.tar.gz
photo-frame-<X.Y.Z>-<short-sha>-arm64.tar.gz.sha256
```

A push that only adds dependency updates keeps the current version and updates
the current draft. Once `vX.Y.Z` is published, later pushes at that version do
nothing; bump the version to start the next release.

### 3. Sign the checksums

Run the **Sign Release Checksums** workflow (`.github/workflows/sign.yml`) with
the release tag, from the Actions UI or:

```bash
gh workflow run sign.yml -f tag=v0.2.0
```

Approve the `release-signing` environment deployment when prompted. The workflow
downloads every `*.sha256` asset from the release, signs each one with
`scripts/sign-checksum.sh`, and uploads the resulting `*.sha256.sig` assets with
`--clobber`.

Putting unrelated `.sha256` assets on the release will make the workflow sign
them too, which is harmless but noisy.

### 4. Verify and publish

Confirm the draft has the expected assets:

```bash
gh release view v0.2.0 --json assets --jq '.assets[].name'
```

Expect both `.deb` files, the bundle, the bundle's `.sha256`, and the
`.sha256.sig`. On a test device, install the HTTPS bundle and then check status
and list:

```bash
SHA=$(awk '{print $1}' photo-frame-0.2.0-abc1234-arm64.tar.gz.sha256)
sudo photo-frame-update install \
  https://github.com/dmikusa/digital-picture-frame/releases/download/v0.2.0/photo-frame-0.2.0-abc1234-arm64.tar.gz \
  --sha256 "$SHA"
sudo photo-frame-update status
sudo photo-frame-update list
```

The install must pass verification, activate, and pass the 30-second health
check. If it fails, the previous release should still be active.

Publish the draft after the signature and a test install are good:

```bash
gh release edit v0.2.0 --draft=false
```

Install it on the frame using the same HTTPS command with the hash from the
published `.sha256` file. Keep the previous release on disk;
`photo-frame-update rollback` returns to it.

### What is automated and what is not

Automated:

- version bump PR creation and auto-merge,
- `.deb` builds for both architectures,
- the arm64 update bundle and its `.sha256`,
- creating or updating the draft release and replacing stale draft assets.

Still manual:

- triggering the version bump workflow and merging the PR,
- running the signing workflow and approving the protected environment,
- reviewing and publishing the draft release,
- installing on the frame.

The build is only for `arm64`; the `amd64` `.deb` is for development and is not
an update target. Signing stays manual so the update key is never available to
the build job.

### Workflow behavior

`sign.yml` has these properties:

- Trigger: `workflow_dispatch` only, with a required `tag` input.
- Environment: `release-signing`.
- Permissions: top level `contents: read`, the sign job `contents: write` so it
  can upload signatures.
- It checks out the repository to get `scripts/sign-checksum.sh`, with
  `persist-credentials: false`.
- It ensures OpenSSL supports `-rawin`, downloading/updating OpenSSL if not.
- It runs `gh release download "$TAG" --pattern '*.sha256' --dir dist`.
- It fails if the release has no checksum assets.
- It uploads `dist/*.sha256.sig` back to the same release.

The `release-signing` environment must be configured before the first run:

1. Create an environment named `release-signing`.
2. Add required reviewers. A signing run then waits for a human approval.
3. Restrict deployment to protected branches or tags as appropriate for the
   repository. The workflow is manual, so the environment is the only gate.
4. Add `UPDATE_SIGNING_KEY` as an environment secret holding the PEM private key.
5. Add `UPDATE_SIGNING_KEY_ID` as an environment secret holding the key ID, for
   example `2026-10`.
6. Pin every action to a full commit SHA. The workflow already pins
   `actions/checkout`.

Pull requests do not run this workflow, and environment secrets are not exposed
to pull requests from forks, so there is no fork path to the key.

### What CI signing does and does not protect

CI is the build-integrity trust anchor. The signing key protects the artifact
between the signing job and the device. It does not make the build trustworthy.

A compromised build machine, a malicious dependency, or a bad commit merged to a
release tag produces a tarball that the signing job will sign without complaint.
The signature proves "this came from our release process", not "this is safe".
Keeping the key in a protected environment with a required reviewer limits the
damage: the key is not present in the build, every use leaves an approval and a
workflow log entry, and a stolen repository token alone cannot sign anything.
That is a real improvement, and it is still not a defence against a compromised
repository or a malicious maintainer.

If that residual risk becomes unacceptable, the next steps are a key that cannot
be exported (cloud KMS or a PKCS#11 HSM) or a self-hosted signing runner on a
machine the build job cannot reach. Both remove key material from GitHub. Neither
removes the fact that whatever gets signed is what the build produced.

## 7. Key rotation

Rotation adds a new key while the old one still works, ships the new key list
inside a release the old key signed, and removes the old key only after the
device has installed that release.

1. Generate a new key pair and give it a new ID, for example `2027-04`.
2. Add the new public key bytes to `TRUSTED_KEYS` in `src/update/keys.rs`,
   keeping the old entry. Rebuild `photo-frame-update`.
3. Build a bundle containing the new updater with both keys.
4. Sign that bundle with the **old** key and publish it as usual.
5. Install it on the device. `photo-frame-update keys` now prints both IDs.
6. Replace `UPDATE_SIGNING_KEY` and `UPDATE_SIGNING_KEY_ID` in the
   `release-signing` environment with the new values.
7. Sign subsequent releases with the new key. Devices that installed step 4
   accept them. A device that skipped step 4 cannot verify them and stays on its
   current release, which is the intended fail-closed behavior.
8. After the overlap, build and sign a release that removes the old entry from
   `TRUSTED_KEYS`. Devices that install it now trust only the new key.
9. Retire the old private key.

The overlap exists because a device can only learn a new key from a release it
already trusts. Keep it long enough to install step 4. There is no expiry date:
a key stays trusted until a release removes it.

## 8. Key compromise, loss, and recovery

### Private key compromised

Rotation through the old key does not help by itself: an attacker with the key
can sign a release that adds their own key, and the device cannot tell that
release from yours.

- Remove the key from the `release-signing` environment and rotate the secret.
- Treat every artifact signed after the suspected leak as hostile.
- If a second, offline-held key is already in `TRUSTED_KEYS`, use it to sign a
  recovery release that installs a clean key list.
- Otherwise, reflash the device from a known-good SD card and install a freshly
  signed release.

There is no online revocation and no expiry to fall back on. The device keeps
running the release it has, so a compromised key does not stop the slideshow; it
only removes your ability to ship a trustworthy update until you recover.

### Private key lost

Nothing breaks on the device. You cannot sign a new release the device will
accept until you restore the key from backup, use a second trusted key, or
reflash with a new key list.

### Recovery key

There is no dedicated recovery mode in `photo-frame-update`. A second key ID in
`TRUSTED_KEYS`, with its private half kept offline and never added to CI, is an
optional manual recovery mechanism: add it ahead of time, then use it to sign a
release that installs a clean key list if the CI key is lost or leaked. This is
not implemented as a separate feature. If you do not provision a second key, the
only path back is a reflash.

### Reflash fallback

Reflashing costs the index, cursors, and any config that was not backed up.
Photos are on their own partition and survive a rootfs reflash if the partition
layout is preserved, but keep a copy of `/etc/photo-frame/config.toml` somewhere
else before you start.

## 9. What is not protected

| Area | Why it is out of scope |
|------|------------------------|
| Kernel, firmware, bootloader | Bundles do not contain them. A kernel CVE is fixed by apt or by the manual kernel update described below. |
| OS packages | libc, OpenSSL, ImageMagick, and everything else in the rootfs come from Debian and DietPi. |
| Physical access to the SD card | The boot partition is unauthenticated, the root filesystem is unencrypted, and the sequence floor is a JSON field. Someone with the card can replace the kernel, edit the key list in the updater binary, or lower the floor and reinstall an old signed release. |
| CI compromise | The signature says the release process produced the artifact, not that the artifact is safe. |
| A malicious maintainer or a merged backdoor | Same trust boundary as CI. |
| Photos at rest | The photos partition is not encrypted. Theft of the card exposes the library. |
| The admin UI in transit | Config mode is plain HTTP on the LAN. It is unchanged by the update tool and remains LAN-only. |
| Denial of service | An attacker who can reach the device can stop it. The update design protects integrity and recoverability, not uptime. |

The anti-rollback floor stops network downgrades and accidental downgrades. It
does not stop a physical attacker, and it should not be described as if it does.

The boot guard is userspace and runs after the app services. It detects an
unhealthy release and reverts it; it cannot prevent that release from starting
once. A corrupt root filesystem or boot partition still needs a reflash.

## 10. OS security updates and kernel cadence

The OS is DietPi on Debian. Security fixes come from apt, not from release
bundles.

`unattended-upgrades` handles Debian security updates:

```bash
sudo apt install -y unattended-upgrades needrestart
sudo dpkg-reconfigure --priority=low unattended-upgrades
```

Configuration that matters in `/etc/apt/apt.conf.d/50unattended-upgrades` and
`20auto-upgrades`:

- Enable the security origin only. Do not pull in stable-updates or backports
  automatically.
- `Unattended-Upgrade::Automatic-Reboot "false";` so a photo frame never reboots
  itself in the middle of the evening.
- Set `needrestart` to automatic mode so services that link a patched library are
  restarted. Do not let it restart the kernel for you:

  ```bash
  echo '$nrconf{restart} = "a";' | sudo tee /etc/needrestart/conf.d/photo-frame.conf
  ```

- Hold the kernel, firmware, and bootloader packages so an unattended run cannot
  change them:

  ```bash
  sudo apt-mark hold raspberrypi-kernel raspberrypi-bootloader \
    linux-image-* firmware-*
  ```

  Package names differ between Raspberry Pi OS releases and plain Debian. Check
  `dpkg -l | grep -E 'raspberrypi|linux-image|firmware'` on the image in use and
  hold the kernel, firmware, and bootloader packages that actually exist there.

There is no custom maintenance timer and no self-written update service. apt does
this job. A second scheduler running `apt-get upgrade` on a timer is one more
thing that can race the first one.

Kernel, firmware, and bootloader updates are manual, roughly twice a year, over
the VPN:

1. Confirm a known-good SD card image is within reach, with the current release
   on it.
2. Connect over the VPN and check free space and the current kernel version.
3. Unhold the kernel, firmware, and bootloader packages.
4. Install the updates, then reboot explicitly.
5. Verify the services, the panel, and the logs.
6. Re-hold the packages.

If the device does not come back, swap in the known-good card and investigate the
old one offline. This is why the manual step exists: a kernel update is the one
change that the boot guard cannot roll back.

## 11. Remote access and remote updates

The frame is administered over a mesh VPN. Tailscale or a self-hosted Headscale
or NetBird deployment all work; the requirement is a private overlay network with
per-device keys, not a port forward.

SSH policy:

- `sshd` listens on loopback and the VPN interface only.
- The firewall drops TCP/22 from the LAN interface. Being on the same Wi-Fi is
  not sufficient to reach SSH.
- `PermitRootLogin no`, `PasswordAuthentication no`, and
  `KbdInteractiveAuthentication no`. Keys only.
- A dedicated `admin` user with its own key. No shared accounts.
- `sudo` is scoped to the updater:

  ```
  admin ALL=(root) NOPASSWD: /usr/local/sbin/photo-frame-update
  ```

  That path is a symlink to `/opt/photo-frame/current/bin/photo-frame-update`,
  so it always runs the active release. Scoping sudo keeps the admin user from
  running arbitrary commands as root, but it does not make the updater harmless:
  the updater installs signed binaries as root, and the signature check is what
  makes that safe.

Config mode's admin server stays LAN-only. It is a setup flow with a rotating
password served over plain HTTP, and it has no business being reachable over the
VPN from a remote network. Do not add a VPN listener for it.

A remote update looks like this:

```bash
scp photo-frame-0.2.0-arm64.tar.gz* admin@photo-frame:
ssh admin@photo-frame
sudo /usr/local/sbin/photo-frame-update install \
  ~admin/photo-frame-0.2.0-arm64.tar.gz
sudo /usr/local/sbin/photo-frame-update status
```

`~admin` is expanded by the admin user's shell before `sudo` runs, so the root
process sees a normal path. All three files are needed: the tarball and both
sidecars.

The health check runs locally, so a dropped VPN connection after the install
starts does not leave the device in a half-activated state. Reconnect and read
`status` and the audit log if the session ends early.

## 12. Troubleshooting and audit

### Where things live

```
/opt/photo-frame/
  releases/<version>/manifest.json
  releases/<version>/bin/photo-frame-manager
  releases/<version>/bin/photo-frame-display
  releases/<version>/bin/photo-frame-update
  current -> releases/<version>
  .staging/                    # root-only, never active

/var/lib/photo-frame/update/
  state.json                   # root-only, mode 0600

/var/log/photo-frame/
  update.log                   # append-only, root-only, mode 0600
```

`state.json` looks like this:

```json
{
  "current": "0.2.0",
  "previous": "0.1.0",
  "last_good": "0.2.0",
  "pending": null,
  "sequence": 7
}
```

A pending install adds:

```json
"pending": {
  "from": "0.1.0",
  "to": "0.2.0",
  "sequence": 7,
  "key_id": "2026-10",
  "source": "https://github.com/dmikusa/photo-frame/releases/download/v0.2.0/photo-frame-0.2.0-arm64.tar.gz",
  "started_at": "2026-10-26T18:12:03Z"
}
```

The `.deb` seeds `releases/<package-version>-seed` from the packaged binaries
with `sequence: 0` and points `current` at it on a fresh install. After that the
updater owns the symlink; a later `.deb` upgrade stages its own release but does
not move `current` back.

### Audit log

`/var/log/photo-frame/update.log` is the record that survives a reboot. One line
per install, rollback, or guard action, with whitespace-free fields:

```
2026-10-26T18:12:03Z action=install from=0.1.0 to=0.2.0 version=0.2.0 sequence=7 key_id=2026-10 source=https://github.com/dmikusa/photo-frame/releases/download/v0.2.0/photo-frame-0.2.0-arm64.tar.gz result=success
```

URL queries and fragments are stripped before logging, and whitespace or control
characters in a value become `_`. Failures add `detail=...`.

The operational logs stay on tmpfs and are lost on reboot:

- `/tmp/photo-frame.log` for the manager
- `/tmp/photo-frame-display.log` for the display app

### Common failures

| Message | Cause |
|---------|-------|
| `no trusted update keys are baked into this build; refusing to verify` | `TRUSTED_KEYS` is empty. Add a key and rebuild the updater. |
| `signature key id '…' is not trusted by this build` | The bundle was signed with a key this build does not have, usually a rotation that has not shipped. |
| `signature verification failed for key '…'` | Wrong key or a corrupted `.sha256.sig`. |
| `SHA-256 mismatch for …` | The tarball does not match the `.sha256` file, or `--sha256` was wrong. |
| `checksum sidecar must contain exactly one line` | The `.sha256` file has extra lines. Regenerate it with `sha256sum`. |
| `release … already exists in /opt/photo-frame/releases` | The same version was installed before. Bump the version. |
| `bundle sequence … is lower than installed sequence …; refusing downgrade` | The sequence is below the floor. Build with a higher sequence. |
| `bundle requires at least version …, but … is installed` | `min_version` is newer than the active release. |
| `--sha256 is required when installing from an HTTPS URL` | Add the digest from the release notes. |
| `only https:// URLs are accepted` / `refusing to follow a redirect to a non-HTTPS URL` | Use the HTTPS release asset URL, or install from a local file. |
| `no previous release is recorded; nothing to roll back to` | Only one release is installed, or state was reset. |
| `previous release … is missing from …` | `gc` or a manual delete removed the fallback. Reinstall a bundle. |
| `previous release … is already current` | `previous` and `current` point at the same release. |
| `… (waited 30s)` | The health check failed. Read the rest of the message, then `state.json` and the tmpfs logs. |
| `release … is unhealthy and there is no previous release to revert to` | The guard could not roll back because no fallback directory exists. |

### If the panel is blank after an update

```bash
systemctl status photo-frame-update-guard.service
sudo photo-frame-update status
sudo cat /var/lib/photo-frame/update/state.json
sudo tail -n 20 /var/log/photo-frame/update.log
```

The guard runs after the app units, so it can revert a release that starts but is
unhealthy. If the guard itself did not run, check that
`photo-frame-update-guard.service` is enabled and that `/opt/photo-frame/current`
resolves to a directory.
