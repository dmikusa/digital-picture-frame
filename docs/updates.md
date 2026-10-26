# Updates

This document describes how a release of the frame is built, signed, verified,
installed, rolled back, and how the signing keys are managed. It covers the
app-only update path: `photo-frame-manager`, `photo-frame-display`, and the
`photo-frame-update` tool itself. The operating system, kernel, firmware, and
bootloader are not part of a release bundle; they follow the apt policy in
[OS security updates](#10-os-security-updates-and-kernel-cadence).

The updater and bundle format below are the agreed design for Phase 1. Until
`photo-frame-update` ships, the only way to change code on a device is to
install a `.deb` from GitHub Releases, as described in
[deployment.md](deployment.md#packaging--installation).

## 1. Overview and trust model

A release is three files: a tarball, a checksum file, and a signature file. The
tarball holds the two application binaries, the updater itself, and a manifest.
The checksum file holds the SHA-256 of the tarball. The signature file holds an
Ed25519 signature over the exact bytes of the checksum file.

The device trusts a fixed list of Ed25519 public keys that are compiled into
`photo-frame-update`. Each key has a short ID such as `release-2026-a`. The
signature file names the key ID it was made with; the updater looks that ID up in
its baked-in list, verifies the signature against the raw 32-byte public key, and
refuses the release if the ID is unknown.

There is no certificate, no certificate authority, no expiry date, no revocation
list, and no network lookup during verification. A signature that was valid in
2026 is still valid in 2036. Everything needed to check a release is either in
the bundle or in the updater binary.

The trust anchor is software only. The Pi Zero 2 W has no secure boot, so
somebody with the SD card can replace `photo-frame-update` and its baked key list
as easily as they can replace the manager. Signing removes the "malicious
network or mirror" and "wrong file" risks. It does not remove the physical-access
risk.

There are no A/B partitions, no RAUC or SWUpdate, and no `dietpi-backup`. The
root filesystem layout stays as it is, and rollback is a userspace symlink flip.

```
GitHub Actions build ──> tarball ──> SHA-256 ──> Ed25519 signature
                                                    │
                                          release assets on GitHub
                                                    │
                     local path or HTTPS ───────────┘
                                │
                    photo-frame-update (baked public keys)
                                │
                     verify, stage, flip, health check
```

## 2. Release bundle layout

Release `0.2.0`, sequence `7`, published as three files:

```
photo-frame-0.2.0-0007.tar.gz
photo-frame-0.2.0-0007.tar.gz.sha256
photo-frame-0.2.0-0007.tar.gz.sha256.sig
```

The version portion of the name is the directory name under
`/opt/photo-frame/releases/`. The sequence is a monotonic integer used as the
anti-rollback floor. It never repeats and never goes backwards, even when the
human-readable version does not change.

The tarball contains a manifest at the root and a `bin/` directory:

```
manifest.json
bin/photo-frame-manager
bin/photo-frame-display
bin/photo-frame-update
```

`manifest.json`:

```json
{
  "bundle_format": 1,
  "product": "photo-frame",
  "arch": "aarch64",
  "version": "0.2.0",
  "sequence": 7,
  "min_version": "0.1.0",
  "config_schema": 1,
  "created": "2026-10-24T18:00:00Z",
  "artifacts": [
    {
      "path": "bin/photo-frame-manager",
      "sha256": "…",
      "size": 4123456,
      "mode": "0755"
    },
    {
      "path": "bin/photo-frame-display",
      "sha256": "…",
      "size": 51200,
      "mode": "0755"
    },
    {
      "path": "bin/photo-frame-update",
      "sha256": "…",
      "size": 2300000,
      "mode": "0755"
    }
  ]
}
```

| Field | Purpose |
|-------|---------|
| `bundle_format` | Refuse a bundle the updater does not understand. |
| `product`, `arch` | Refuse a bundle built for another product or CPU. |
| `version` | Human-readable version; names the release directory. |
| `sequence` | Monotonic counter for the anti-rollback floor. |
| `min_version` | Refuse if the installed version is older than the bundle supports. |
| `config_schema` | Config schema the app expects; drives migration. |
| `artifacts[]` | Path, SHA-256, size, and mode for every installed file. |

The `.sha256` file is standard `sha256sum` output. Note the two spaces between
the digest and the file name:

```
9c1f0b6d…  photo-frame-0.2.0-0007.tar.gz
```

The `.sha256.sig` file is a small JSON envelope holding the raw signature:

```json
{"key_id":"release-2026-a","signature":"iV7K…"}
```

`signature` is the base64url encoding of 64 raw Ed25519 signature bytes, with no
padding. It signs the exact bytes of the `.sha256` file, not the tarball. The
chain is deliberate: the signature authenticates the checksum, and the checksum
authenticates the tarball, so both files are required.

Putting the signature in a JSON envelope rather than a binary sidecar makes the
key ID explicit and keeps the file readable and diffable. The cost is one extra
parse step, which happens after the size check and before any extraction.

## 3. Commands

```
photo-frame-update install <path|url> [--sha256 <hex>] [--force] [--verify-only]
photo-frame-update list
photo-frame-update status
photo-frame-update rollback [<version>]
photo-frame-update gc
photo-frame-update guard [--clear]
photo-frame-update keys
```

`install`, `rollback`, `gc`, and `guard --clear` need root. `status` and
`guard` read root-owned state, so run them with `sudo` as well. `list` and `keys`
work for any user.

### Local install

Copy all three files into one directory. The sidecars must sit next to the
tarball; the updater does not look anywhere else and does not accept a bundle
without them.

```bash
sudo photo-frame-update install /media/usb0/photo-frame-0.2.0-0007.tar.gz
```

An offline install is the normal path for a device with no network, and it is
also the path to use when you have downloaded a release on a laptop.

### HTTPS install

```bash
sudo photo-frame-update install \
  https://github.com/dmikusa/photo-frame/releases/download/v0.2.0/photo-frame-0.2.0-0007.tar.gz \
  --sha256 9c1f0b6d…
```

`--sha256` is required for a URL install. The updater fetches the tarball, then
fetches the same URL with `.sha256` and `.sha256.sig` appended, verifies the
signature, and checks the tarball against both the sidecar and the digest you
passed. The out-of-band digest catches "this is a validly signed release, but
not the one I meant to install", which a signature alone cannot catch.

Plain `http://` URLs are refused. A redirect from HTTPS to HTTP is refused as
well. The digest is the only thing the transport is trusted for, and only
because it is pinned outside the download.

### Inspecting and managing releases

```bash
$ photo-frame-update list
0.2.0-0007  current    2026-10-24T18:12:03Z
0.1.0-0003  previous   2026-10-11T09:41:55Z
0.1.0-0002             2026-09-30T20:02:11Z
```

```bash
$ photo-frame-update status
current:   0.2.0-0007
previous:  0.1.0-0003
pending:   none
last:      install 0.2.0-0007 result=success key=release-2026-a
guard:     armed, no revert scheduled
```

`rollback` flips `current` back to the previous release, or to a named release
that is still on disk. It runs the same health check and writes the same audit
entry as an install. Rolling back to a version below the stored sequence floor
requires `--force` and is recorded as a forced action.

`gc` trims the release directory. It keeps the current release, the previous
release, and the four most recent releases, so at most six directories remain.
Anything older is removed. `gc` runs automatically after a successful install and
can also be run by hand.

`guard` reports what the boot guard will do on the next boot. If a pending
update is stuck because the unit was powered off at the wrong moment,
`guard --clear` removes the marker without reverting. Use it only when you know
the current release is good.

`keys` prints the baked key list with each key's fingerprint:

```bash
$ photo-frame-update keys
release-2026-a  SHA256:9f2c1d…  active
```

The fingerprint is the SHA-256 of the raw 32-byte public key. It is the value to
compare against the release notes and against the signing machine.

## 4. Install, activation, health check, rollback, boot guard

An install never writes over a running binary. It stages, verifies, extracts a
new directory, then changes one symlink.

1. Copy the tarball and both sidecars into
   `/var/lib/photo-frame/update/staging/` (mode 0700), writing to `*.part` and
   renaming when the copy is complete. A partial copy is never a candidate.
2. Verify the signature over the `.sha256` bytes using the key named by
   `key_id`. Unknown key ID or a bad signature ends the install here.
3. Compare the tarball's SHA-256 with the checksum file, and with `--sha256`
   when it was given.
4. Parse `manifest.json`. Check `bundle_format`, `product`, `arch`,
   `min_version`, and `config_schema`. Compare `sequence` with the stored floor;
   refuse anything at or below it unless `--force` was passed.
5. Check free space, then extract into
   `/opt/photo-frame/releases/<version>.new`. Extraction rejects absolute paths,
   `..`, symlinks, hardlinks, device nodes, and duplicate entries. Verify each
   `artifacts[]` hash and size before anything is activated.
6. Rename `<version>.new` to `<version>`. Write the pending marker with the
   source version, target version, and attempt count.
7. Create `current.new` and `rename()` it over `current`. That rename is the
   activation point.
8. `systemctl restart photo-frame-display photo-frame-manager`.
9. Run the health check for up to 90 seconds.
10. On success, clear the pending marker, write `last-result.json`, append to the
    audit log, and show the new version on the panel.
11. On failure, point `current` back at the previous release, restart, record
    the failure, and show a message on the panel. The device keeps running the
    old version.

Health checks:

| Check | Pass condition |
|-------|----------------|
| Services | `systemctl is-active` for both units |
| Crash loop | `NRestarts` stays under the threshold |
| Manager liveness | `/run/photo-frame/healthy` exists and is recent |
| Display socket | `/run/photo-frame/photo-frame.sock` exists and is a socket |
| Control socket | `/run/photo-frame/control.sock` exists and is a socket |
| Photos | Photos directory is mounted and readable |

`photo-frame-boot-guard.service` is a `oneshot` unit ordered before the two app
units. It covers the case where the new release cannot start far enough to fail a
health check, and the case where power is lost between the symlink flip and the
confirmation write.

1. No pending marker: exit.
2. Pending marker present and unconfirmed, first boot since the flip: increment
   the attempt count and exit.
3. Pending marker present and already attempted once: revert `current` to the
   recorded source version, restart the units, and record the revert.
4. Exit; the app units start against a known-good release.

Because the kernel and boot files never change, the boot guard always runs. That
is what makes app-only rollback reliable without a bootloader or a second root
partition. A corrupt root filesystem or boot partition is outside this
mechanism's reach.

## 5. Generating a signing key and baking the public key

Keys are Ed25519. OpenSSL can generate and use them without extra tooling, which
keeps the signing procedure to a handful of commands that are easy to reproduce.

Generate a key pair on the signing machine:

```bash
openssl genpkey -algorithm ED25519 -out update-signing-2026-a.pem
chmod 600 update-signing-2026-a.pem
```

The private key is a small PEM file. Keep it in a password manager or an
encrypted volume, never in the repository and never on the frame. Back it up. A
lost key cannot sign anything the device will accept; recovery means a recovery
key or a reflash.

Derive the raw public key. The last 32 bytes of the DER SubjectPublicKeyInfo are
the key itself:

```bash
openssl pkey -in update-signing-2026-a.pem -pubout -outform DER \
  | tail -c 32 | xxd -p -c 32
```

The fingerprint to compare against `photo-frame-update keys` is the SHA-256 of
those same 32 bytes:

```bash
openssl pkey -in update-signing-2026-a.pem -pubout -outform DER \
  | tail -c 32 | sha256sum
```

That hex string is what goes into the updater source. The key list is a compiled
constant keyed by ID:

```rust
// src/update/keys.rs
pub const TRUSTED_KEYS: &[TrustedKey] = &[
    TrustedKey {
        id: "release-2026-a",
        public_key: [
            0x9f, 0x2c, 0x1d, /* … 32 bytes total … */
        ],
    },
];
```

Add a unit test that decodes each key to exactly 32 bytes and prints its
SHA-256 fingerprint. Compare that fingerprint with the signing machine's output
before tagging a release; a single wrong byte produces a signature that can never
verify and is hard to spot by eye.

Because the key list lives in the binary, the trusted set can only change through
an update. There is no separate public-key file installed by apt and no key
directory to edit. This is the point: an attacker who can write
`/etc/photo-frame/` still cannot add a key. They have to replace the updater
binary, which is the same as replacing the app.

Sign a release:

```bash
cd dist/

openssl pkeyutl -sign -rawin \
  -inkey ../update-signing-2026-a.pem \
  -in photo-frame-0.2.0-0007.tar.gz.sha256 \
  -out signature.bin

sig=$(openssl base64 -A -in signature.bin | tr '+/' '-_' | tr -d '=')
printf '{"key_id":"release-2026-a","signature":"%s"}\n' "$sig" \
  > photo-frame-0.2.0-0007.tar.gz.sha256.sig

rm signature.bin
```

Verify the result with the same code the device uses:

```bash
sudo photo-frame-update install --verify-only \
  photo-frame-0.2.0-0007.tar.gz
```

Then publish all three files on the GitHub release for the tag.

## 6. CI signing workflow and GitHub environment setup

Two jobs, with the key available to only one of them.

`build` compiles the tarball, computes the checksum, and uploads both as workflow
artifacts. It has no access to any signing secret and only needs
`contents: read` plus `actions: write`.

`sign` downloads the artifacts, imports the key, signs the checksum, and uploads
the signed set. It runs only for a protected tag or a manual dispatch, uses the
`release-signing` environment, and has `contents: read` plus `actions: write`.

`publish` downloads the signed set and uploads it to the release. It never sees
the key. It is the only job with `contents: write`.

```yaml
name: release

on:
  push:
    tags: ["v*"]
  workflow_dispatch:
    inputs:
      tag:
        description: Release tag to publish to
        required: true

permissions:
  contents: read

jobs:
  build:
    runs-on: ubuntu-22.04-arm
    permissions:
      contents: read
      actions: write
    steps:
      - uses: actions/checkout@<sha>
        with:
          persist-credentials: false
      # … build, tar, sha256sum …
      - uses: actions/upload-artifact@<sha>
        with:
          name: bundle
          path: dist/photo-frame-*.tar.gz*

  sign:
    needs: build
    runs-on: ubuntu-22.04
    environment: release-signing
    permissions:
      contents: read
      actions: write
    steps:
      - uses: actions/download-artifact@<sha>
        with:
          name: bundle
          path: dist
      - name: Sign checksum
        env:
          UPDATE_SIGNING_KEY: ${{ secrets.UPDATE_SIGNING_KEY }}
          UPDATE_SIGNING_KEY_ID: ${{ secrets.UPDATE_SIGNING_KEY_ID }}
        run: |
          set -euo pipefail
          key="$RUNNER_TEMP/update-signing.pem"
          umask 077
          printf '%s\n' "$UPDATE_SIGNING_KEY" > "$key"
          trap 'rm -f "$key"' EXIT

          cd dist
          openssl pkeyutl -sign -rawin -inkey "$key" \
            -in photo-frame-*.tar.gz.sha256 -out "$RUNNER_TEMP/sig.bin"

          sig=$(openssl base64 -A -in "$RUNNER_TEMP/sig.bin" \
            | tr '+/' '-_' | tr -d '=')
          printf '{"key_id":"%s","signature":"%s"}\n' \
            "$UPDATE_SIGNING_KEY_ID" "$sig" \
            > photo-frame-*.tar.gz.sha256.sig
      - uses: actions/upload-artifact@<sha>
        with:
          name: signed-bundle
          path: dist/photo-frame-*.tar.gz*

  publish:
    needs: sign
    runs-on: ubuntu-22.04
    permissions:
      contents: write
      actions: read
    steps:
      - uses: actions/download-artifact@<sha>
        with:
          name: signed-bundle
          path: dist
      - run: gh release upload "$TAG" dist/* --clobber
        env:
          GH_TOKEN: ${{ github.token }}
          GH_REPO: ${{ github.repository }}
          TAG: ${{ inputs.tag || github.ref_name }}
```

`workflow_dispatch` takes the tag as an input because the dispatch ref is a
branch, and the publish step needs a release to upload to. Either trigger still
blocks on the environment's required reviewer.

One-time setup in the repository settings:

1. Create an environment named `release-signing`.
2. Add required reviewers. Every signing job then waits for a human approval.
3. Restrict deployment to protected branches and tags, with a tag rule `v*`. The
   environment, and therefore the secrets, is not available to pull requests,
   including pull requests from forks.
4. Add `UPDATE_SIGNING_KEY` as an environment secret holding the PEM private key.
5. Add `UPDATE_SIGNING_KEY_ID` as an environment secret or variable holding the
   key ID, for example `release-2026-a`.
6. Pin every action to a full commit SHA. Tags can move; SHAs cannot.
7. Keep `contents: write` out of the build and sign jobs.

The protection rules matter as much as the secret. A tag push that bypasses the
required reviewer or the tag rule is a signing event without a human in the loop.

### What CI signing does and does not protect

CI is the build-integrity trust anchor. The signing key protects the artifact
between the build and the device. It does not make the build trustworthy.

A compromised build job, a malicious dependency, or a bad commit merged to a
release tag produces a tarball that the sign job will sign without complaint. The
signature proves "this came from our workflow", not "this is safe". The split into
two jobs with a dedicated key limits the damage: a compromised build job alone
cannot sign anything, the key is not present in the job that runs the compiler,
and every use of the key leaves an approval and an Actions log entry. That is a
real improvement over signing in the build job, and it is still not a defence
against a compromised repository or a malicious maintainer.

If that residual risk ever becomes unacceptable, the next steps are a key that
cannot be exported (cloud KMS or a PKCS#11 HSM), or a self-hosted signing runner
on a machine the build job cannot reach. Both remove key material from GitHub.
Neither removes the fact that whatever the build job produces will be signed.

## 7. Key rotation

Rotation adds a new key while the old one still works, ships the new key list
inside a release the old key signed, and removes the old key only after devices
have had time to install that release.

1. Generate a new key pair and give it a new ID, for example `release-2027-a`.
2. Add the new public key bytes to `TRUSTED_KEYS` in `photo-frame-update`. Keep
   the old entry. Bump the tool's version.
3. Build a release signed with the **old** key that contains the new updater with
   both keys baked in. Publish it as usual.
4. Wait until the device (or every device you care about) runs that release.
   `photo-frame-update keys` shows both IDs.
5. Replace the CI secrets with the new key, then sign subsequent releases with
   it. Devices that installed step 3 accept them. A device that skipped step 3
   cannot verify them and stays on its current release, which is the intended
   fail-closed behavior.
6. After the overlap, ship a release signed with the new key that removes the old
   entry from `TRUSTED_KEYS`. Devices that install it now trust only the new key.
7. Retire the old private key and delete the old CI secret.

During the overlap both keys verify. Keep the overlap long enough to install the
transition release, but not so long that a leaked old key stays useful. For a
single-device project the overlap can be one release cycle.

Nothing expires. A key that stays in the list stays trusted forever, so removing
it is a deliberate step, not something the calendar does.

## 8. Key compromise, loss, and recovery

### Private key compromised

If the key may have leaked, rotation does not help by itself: an attacker with
the key can sign a release that adds their own key, and a device cannot tell that
release from yours.

- Remove the key from CI immediately and rotate the environment secrets.
- Treat every artifact signed after the suspected leak as hostile.
- If a recovery key was baked in at provisioning (see below), sign a recovery
  release with it and use that to install a clean key list.
- Otherwise, reflash the device from a known-good SD card image and reinstall a
  freshly signed release.

There is no online revocation and no expiry to fall back on. The device keeps
running the release it has, so a compromised key does not stop the slideshow; it
only removes your ability to ship a trustworthy update until you recover.

### Private key lost

Nothing breaks on the device. You simply cannot sign a new release that the
device will accept. Options, in order of preference:

1. Restore the key from the backup you made when it was generated.
2. Use the recovery key, if one was provisioned.
3. Reflash the device with a build that contains a new key list.

Losing the key is an inconvenience, not an outage. Losing the only backup and the
recovery key means the device is frozen at its current release until it is
reflashed.

### Recovery key

A second Ed25519 key can be generated at provisioning time and baked into
`photo-frame-update` alongside the release key. Its private half is kept offline
and is used only for recovery releases. It is never added to CI. The cost is one
more 32-byte constant and one more line in the rotation procedure. For a project
with a single device, the honest alternative is to accept the reflash fallback
and skip the recovery key; document which choice was made so nobody assumes a
recovery path that does not exist.

### Reflash fallback

Reflashing is the last resort and is expected to be rare. It costs the index,
cursors, and any config that was not backed up. Photos are on their own partition
and survive a rootfs reflash if the partition layout is preserved, but keep a
copy of `/etc/photo-frame/config.toml` somewhere else before you start.

## 9. What is not protected

| Area | Why it is out of scope |
|------|------------------------|
| Kernel, firmware, bootloader | Bundles do not contain them. A kernel CVE is fixed by apt or by the manual kernel update described below, not by `photo-frame-update`. |
| OS packages | libc, OpenSSL, ImageMagick, and everything else in the rootfs come from Debian and DietPi. |
| Physical access to the SD card | The boot partition is unauthenticated, the root filesystem is unencrypted, and the sequence floor is a file. Someone with the card can replace the kernel, edit the key list in the updater binary, or lower the floor and reinstall an old signed release. |
| CI compromise | Covered in section 6. The signature says the workflow produced the artifact, not that the artifact is safe. |
| A malicious maintainer or a merged backdoor | Same trust boundary as CI. |
| Photos at rest | The photos partition is not encrypted. Theft of the card exposes the library. |
| The admin UI in transit | Config mode is plain HTTP on the LAN. It is unchanged by the update work and remains LAN-only. |
| Denial of service | An attacker who can reach the device can stop it. The update design protects integrity and recoverability, not uptime. |

The anti-rollback floor stops network downgrades and accidental downgrades. It
does not stop a physical attacker, and it must not be described as if it does.

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
- Install `needrestart` and set it to automatic mode so services that link a
  patched library are restarted. Do not let it restart the kernel for you:

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
  admin ALL=(root) NOPASSWD: /opt/photo-frame/current/bin/photo-frame-update
  ```

  Note that this is real privilege. The updater installs signed binaries as root.
  Scoping sudo to it keeps the admin user from running arbitrary commands as
  root, but it does not make the updater harmless. The signature check is what
  makes it safe.

Config mode's admin server stays LAN-only. It is a setup flow with a rotating
password served over plain HTTP, and it has no business being reachable over the
VPN from a remote network. Do not add a VPN listener for it.

A remote update looks like this:

```bash
scp photo-frame-0.2.0-0007.tar.gz* admin@photo-frame:
ssh admin@photo-frame
sudo /opt/photo-frame/current/bin/photo-frame-update install \
  ~admin/photo-frame-0.2.0-0007.tar.gz
sudo /opt/photo-frame/current/bin/photo-frame-update status
```

The absolute path is the one in the sudoers rule. `~admin` is expanded by the
admin user's shell before `sudo` runs, so the argument is a normal path by the
time the root process sees it.

The health check runs locally, so a dropped VPN connection after the install
starts does not leave the device in a half-activated state. Reconnect and read
`status` and the audit log if the session ends early.

## 12. Troubleshooting and audit

State lives in two places:

```
/opt/photo-frame/
  releases/
    0.2.0-0007/
      bin/photo-frame-manager
      bin/photo-frame-display
      bin/photo-frame-update
    0.1.0-0003/
  current -> releases/0.2.0-0007

/var/lib/photo-frame/update/
  sequence          # monotonic floor
  pending.json      # in-flight update, if any
  last-result.json  # last install or rollback result
  audit.log         # append-only, root-owned, 0600
  staging/          # completed downloads before extraction
```

`audit.log` is the record that survives a reboot. One line per attempt:

```
2026-10-24T18:12:03Z action=install from=0.1.0-0003 to=0.2.0-0007
  seq=7 bundle_sha256=9c1f0b6d… key_id=release-2026-a
  source=https trigger=cli result=success
```

It is separate from the operational logs, which stay on tmpfs:

- `/tmp/photo-frame.log` for the manager
- `/tmp/photo-frame-display.log` for the display app

Those are lost on reboot, which is why the audit log exists.

Common failures and what they mean:

| Message | Cause |
|---------|-------|
| `signature key id not trusted` | The bundle was signed with a key this build does not have, usually a rotation that was not shipped yet. |
| `signature verification failed` | Wrong key, corrupted `.sha256.sig`, or a hand-edited checksum file. |
| `checksum mismatch` | The tarball does not match the `.sha256` file, or `--sha256` was wrong. |
| `refusing non-HTTPS URL` / `redirect to non-HTTPS` | The URL or a redirect is plain HTTP. Use the release asset URL on GitHub. |
| `version not newer` | The sequence is at or below the floor. Pass `--force` only for an intentional rollback. |
| `health check failed, reverted` | The new release did not come up. Read `last-result.json` and the tmpfs logs for the reason. |
| `no release to roll back to` | Only one release is installed. Reinstall from a bundle. |

If the panel is blank after an update, the boot guard is the first thing to check:

```bash
systemctl status photo-frame-boot-guard.service
sudo photo-frame-update status
sudo cat /var/lib/photo-frame/update/last-result.json
```

The boot guard runs before the app units, so it can revert even when the
manager cannot start.
