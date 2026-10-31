# Photo Frame — Device Update Strategy

## How to use this document

This is a planning document, not a specification. It defines three concrete
update plans, compares them, and recommends a staged path. It is written to be
turned into issues: each phase and task below can become a GitHub issue with the
IDs given.

It is a **living document**. Revise it when a phase lands, when the threat model
(`plans/UPDATE_THREAT_MODEL.md`) changes, or when the owner answers an open
question. Keep the comparison table and the requirements mapping current.

The design has since been decided. The decision record is Section 15, and the
operational documentation is `docs/updates.md`. Where this plan and
`docs/updates.md` disagree, `docs/updates.md` is correct. Sections 4 and 5 are
kept as the analysis that led to the decision; the A/B OS update options in them
were evaluated and **not** adopted.

Companion: `plans/UPDATE_THREAT_MODEL.md` holds the findings (`T-S*`, `T-T*`,
`T-R*`, `T-I*`, `T-D*`, `T-E*`) and the MUST/SHOULD/NICE requirements
(`M1`–`M9`, `S1`–`S7`, `N1`–`N5`) referenced throughout.

---

## 1. Executive summary

The frame currently updates by reflashing the SD card. That is not an update
path: it wipes `config.toml` (and its cloud credentials), the index, cursors,
hostname, Wi-Fi, and SSH identity, and it requires a computer and physical
access. The device also has no hardware secure boot, no A/B boot slots, and no
`tryboot`/EEPROM fallback, so any update design must supply its own
authenticity, rollback, and recovery.

The approach that was adopted is **Option A: app-only signed release bundles**.
The planning recommendation had been Option C, which is A now with the door left
open for B later; the owner chose app-only, so B is not built. Option A:

- updates the two application binaries plus the updater — not the OS or kernel;
- installs from a signed bundle verified against public keys **baked into
  `photo-frame-update`** before anything privileged happens;
- uses versioned install directories plus an atomic `current` symlink flip;
- health-checks the new version and **automatically reverts** on failure, with a
  userspace boot guard that covers power loss and a new version that will not
  start;
- installs from a local file or an HTTPS URL, with the checksum and signature
  sidecars verified in both cases;
- never turns the frame into a permanently listening service; and
- preserves all state by construction, because it never touches the photos
  partition or `/etc/photo-frame`.

Option B (full OS A/B images) is the only way to patch the kernel, ImageMagick,
OpenSSL, and the OS. It is large, changes the partition layout, and on this
board still needs a bootloader (U-Boot) or a recovery kernel to make rollback
work at boot.

**The decision:** app-only updating is sufficient. The OS, kernel, firmware, and
bootloader are maintained with `unattended-upgrades` plus a manually scheduled
kernel/firmware refresh about twice a year, not with an A/B image mechanism.
Option B is not being built, and RAUC, SWUpdate, and `dietpi-backup` are not part
of the design. See `docs/updates.md` for the procedure and Section 15 for the
full decision record.

---

## 2. Requirements

These are the acceptance criteria for any option, taken from Section 6 of
`plans/UPDATE_THREAT_MODEL.md`. Section 11 maps each option against them.

### Must

| ID | Requirement | Source finding |
|----|-------------|----------------|
| M1 | Authenticated updates: every artifact is verified on-device against a key the update channel cannot obtain or replace; check before any privileged action; unsigned/invalid → refuse, log, no partial install. | T-S1, T-S2, T-T1, T-E1 |
| M2 | Integrity covers the whole artifact set (binaries, units, and any scripts run as root), not just a manifest. | T-T1, T-E1 |
| M3 | No root execution without verification; no "install then verify." | T-E1 |
| M4 | Crash/power-fail safety and a rollback path: an interrupted or bad update must not brick the device, must not depend on network or display to recover, and must be able to boot the previous version or a recovery mode. | T-D1, T-D2, T-D3 |
| M5 | Fail-closed: if verification, key handling, or version checks fail, keep running the current version. | T-S1, T-E1 |
| M6 | Anti-rollback as far as the platform allows: reject versions below a persisted floor, and be explicit that a physical/SD attacker can defeat a software-only floor. | T-E6, T-D3 |
| M7 | Preserve state: photos partition, `config.toml`, index/cursors/status, Wi-Fi/SSH config, and host keys survive; `config.toml` is never clobbered by a package default. | T-I1, T-D4 |
| M8 | Least privilege for the update path; not reachable or triggerable by an unauthenticated LAN actor. | T-E4, T-D5, T-D6 |
| M9 | Offline-capable: verification and install work from USB/local media with no internet; signing must not rely on an online OCSP/CA check. | T-S2, constraint |

### Should

| ID | Requirement | Source finding |
|----|-------------|----------------|
| S1 | Explicit opt-in; no silent auto-update; checks and installs are owner-initiated or owner-approved. | T-I5, constraint |
| S2 | Minimal, opt-out update checks with no device-identifying telemetry (no stable ID, coarse version only, no heartbeat). | T-I5 |
| S3 | Signed metadata with a monotonic sequence and optional freshness date to defend against replay/freeze. | T-E6, T-S2 |
| S4 | Audit log on persistent storage (not just tmpfs): version before/after, source, key id, result. | T-R1, T-R2 |
| S5 | Key rotation and revocation with a documented offline process; support a new key without a reflash. | T-E2 |
| S6 | Update-path sandboxing/atomicity: verify and stage separately, then swap; never write the running root in place. | T-D1, T-E1 |
| S7 | Recovery tooling documented for the owner (offline image inspection, recover without losing photos). | T-D1, T-D2 |

### Nice

| ID | Requirement | Source finding |
|----|-------------|----------------|
| N1 | Reproducible builds and published hashes/SBOM so the owner can verify CI output. | B4, T-E1 |
| N2 | Optional dm-verity / read-only root (tamper-evident, not tamper-proof, given no secure boot). | T-T2 |
| N3 | Encrypted photos partition (LUKS, key off the SD card) to protect A1 on theft. | T-I4 |
| N4 | Owner-run rollback command and a documented "hold" to pin a known-good version. | T-E6 |
| N5 | Sign CI artifacts at build time with a key not exposed to the general build job, or publish checksums signed out-of-band. | T-S1, T-E2 |

### Derived implementation controls

A few controls the strategy relies on are not separate numbered requirements in
the threat model. They implement M2/S6 (safe extraction) and M7 (config
versioning):

| Control | Derives from |
|---------|--------------|
| Bundle extraction rejects traversal, symlinks, hardlinks, devices, and absolute paths. | M2, S6 |
| Config schema versioning (`config_version`) with forward migration. | M7 |
| Health check covering both services, sockets, and index availability. | M4, S7 |
| Failures visible on the panel and in the audit log. | M4, S4 |

---

## 3. Option A — App-only signed release bundles (adopted)

### 3.1 What is updated

| Updated | Not updated |
|---------|-------------|
| `photo-frame-manager` (Rust) | OS packages, libc, OpenSSL |
| `photo-frame-display` (C) | Kernel, firmware, boot files |
| `photo-frame-update` helper | ImageMagick and other runtime deps |
| | Partition layout |
| | systemd units (installed once by the `.deb`) |
| | docs, `display.env`, `config.toml` |

This is deliberately narrow. It fixes application bugs and ships features. It
cannot fix a kernel CVE, an ImageMagick CVE, or an OpenSSL CVE; those follow the
apt and manual kernel policy in `docs/updates.md`.

### 3.2 Signed bundle format

Three files per release. Keep it simple and auditable.

```
photo-frame-0.2.0-arm64.tar.gz
photo-frame-0.2.0-arm64.tar.gz.sha256
photo-frame-0.2.0-arm64.tar.gz.sha256.sig
```

The name is `photo-frame-<version>-<arch>.tar.gz`. The sequence is in the
manifest, not the filename. The tar contains a manifest at the root and a `bin/`
directory:

```
manifest.json
bin/photo-frame-manager
bin/photo-frame-display
bin/photo-frame-update
```

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

| Field | Purpose |
|-------|---------|
| `format` | Refuse formats the updater does not understand. Only `1` is accepted. |
| `version` | Release directory name; must be a safe single path component. |
| `sequence` | Monotonic integer; the anti-rollback floor (M6, S3). |
| `arch` | Refuse bundles for another CPU; only `arm64` and `aarch64` are accepted. |
| `min_version` | Optional. Refuse if the active `current` is older than the bundle supports. |
| `files[]` | Path and SHA-256 for every installed file (M2). |

There is no `product`, `size`, or `mode` field. The extractor sets `0755` for
files under `bin/` and `0644` for everything else.

The `.sha256` file is `sha256sum` output. The `.sha256.sig` file is a JSON
envelope over the exact bytes of the checksum file:

```json
{"key_id":"2026-10","signature":"<base64url>"}
```

**Why the signature covers the checksum file** rather than a field inside the
manifest: the device can verify one small file before it parses any untrusted
JSON, the manifest stays a plain description of contents, and the tarball is
bound through its digest. Both sidecars are required; the updater refuses a
bundle without them.

Extraction has hard caps (100 MiB bundle, 512 MiB extracted, 10,000 entries) and
an allowlist: regular files and directories only, with no absolute paths, `..`,
links, or device nodes.

### 3.3 Signing

**Ed25519, raw signature, no certificate.** OpenSSL generates the key and the
repository's `scripts/sign-checksum.sh` produces the envelope:

```bash
openssl genpkey -algorithm ED25519 -out update-signing-2026-10.pem
UPDATE_SIGNING_KEY="$(cat update-signing-2026-10.pem)" \
UPDATE_SIGNING_KEY_ID=2026-10 \
  scripts/sign-checksum.sh dist/update/photo-frame-0.2.0-arm64.tar.gz.sha256
```

There is no CA, no certificate chain, no expiry, and no network check, so the
device can verify offline. The private key file is stored in a password manager
or encrypted volume, and its backup is kept separately. Signing runs in CI, in
a manual workflow with its own protected environment; the private key is not on
the device and is not in the build. Section 7.2 covers the split and its limits.

The public key is **baked into `photo-frame-update`** as a compiled constant, not
installed as a file. Section 7.3 covers distribution; `docs/updates.md` covers
the commands.

### 3.4 On-device verification (before any privileged action)

The updater runs as root (it must replace binaries and flip the symlink) but
performs all of this **before** touching the live install:

1. Verify the signature over the `.sha256` bytes with the key named by `key_id`.
   **Fail closed** on an empty compiled key list, an unknown key ID, or an
   invalid signature (M1, M5).
2. Copy (local) or download (HTTPS) the bundle into
   `/opt/photo-frame/.staging` (0700) and hash that copy. An interrupted copy is
   never a candidate.
3. Check the copied bundle's SHA-256 against the checksum file, and against
   `--sha256` when it was supplied. `--sha256` is required for URL installs.
4. Extract into a fresh directory under `.staging` and parse `manifest.json`.
   Check `format`, `version`, `arch`, and `min_version` (M3).
5. Compare `sequence` with the stored floor; refuse anything lower. An equal
   sequence is allowed only when the target release directory does not already
   exist. There is no `--force` (M6).
6. Verify each `files[]` entry's SHA-256 against the extracted payload (M2).
7. Only now rename the staged tree into `releases/<version>` and proceed
   (Section 3.5).

### 3.5 Versioned installs and the flip

```
/opt/photo-frame/
  releases/
    0.1.0/                 # previous, kept for rollback
      manifest.json
      bin/photo-frame-manager
      bin/photo-frame-display
      bin/photo-frame-update
    0.2.0/                 # current
  current -> releases/0.2.0
  .staging/                # root-only, never active
/var/lib/photo-frame/update/
  state.json               # current, previous, last_good, pending, sequence (0600)
/var/log/photo-frame/
  update.log               # persistent audit log, root-owned, 0600
```

The systemd units are installed once by the `.deb` and point at the stable path
`/opt/photo-frame/current/bin/...`. A release only contains the three binaries,
so an update just flips the symlink and no unit changes are involved. If a unit
ever has to change, that is a manual `.deb` upgrade, not a bundle.

The `.deb` also seeds `releases/<package-version>` from its packaged binaries at
sequence 0, so a fresh device has a release to run before the first bundle.

Retention keeps the current release, the previous release, and the four most
recently modified releases; `gc` removes anything else after a successful
install.

Install sequence (all on ext4, all atomic via `rename()`):

1. Verify and extract under `.staging` (Section 3.4).
2. `rename()` the staged tree to `releases/<version>` and flush the directory.
3. Write `state.json` with the pending marker (`from`, `to`, `sequence`,
   `key_id`, `source`, `started_at`) via a temp file, `rename`, and `fsync`.
4. Create a temporary symlink, then `rename()` it over `current`.
5. `systemctl restart photo-frame-display.service photo-frame-manager.service`.
6. Health check (Section 3.6).
7. On success: set `current`, `previous`, `last_good`, and the sequence floor;
   clear `pending`; append to `update.log`; run `gc`.
8. On failure: revert `current` to the previous release if it exists and
   restart; otherwise remove `current`. Clear `pending`, record the failure, and
   return an error.

### 3.6 Health check

Run for up to 30 seconds after restart, probing every 500 ms:

| Check | Pass condition |
|-------|----------------|
| Services | `systemctl is-active --quiet` succeeds for `photo-frame-display.service` and `photo-frame-manager.service` |
| Display socket | `/run/photo-frame/photo-frame.sock` exists and is a socket |
| Control socket | `/run/photo-frame/control.sock` exists and is a socket |

There is no `healthy` file, no restart-count check, and no photos-directory
check. The socket paths are hardcoded, so moving them in `config.toml` and
`display.env` makes a post-update health check fail.

### 3.7 Boot guard (userspace rollback)

There is no hardware fallback, so recovery must work even if the new version
cannot start. `photo-frame-update-guard.service` is a `oneshot` unit ordered
`After=photo-frame-manager.service photo-frame-display.service` and
`Wants=` both:

1. No `pending` in `state.json`: exit.
2. If `pending` exists and neither the `current` symlink nor `state.current`
   names `pending.to`, the release never activated: clear `pending` and record
   `cleared`.
3. Otherwise wait `--grace-seconds 30`, run the health check, and either confirm
   the release (update `current`, `previous`, `last_good`, sequence, clear
   `pending`) or revert `current` to `pending.from` and restart.
4. If there is no usable previous release on failure, clear `pending` and record
   `failed`.

Because the kernel is unchanged, the boot guard always runs. Because it runs
after the app units, it cannot stop a broken release from starting once; it
detects the failure and rolls back. There is no attempt counter and no
`guard --clear`. The only unrecoverable case is a corrupt rootfs or boot
partition, which Option A does not touch.

### 3.8 Offline install

1. The owner copies the tarball and both sidecars to a FAT-formatted USB stick.
2. Inserting the stick is detected by the existing `/media` watcher, which mounts
   it as usual. Photo import runs; the update files are ignored by the importer
   because they are not images.
3. The owner installs from a shell (SSH or the console):

   ```bash
   sudo photo-frame-update install \
     /media/usb0/photo-frame-0.2.0-arm64.tar.gz
   ```

   The sidecars must be in the same directory. A bundle without them is refused.
4. The updater prints the result and writes the audit log.

There is no auto-apply marker directory in the adopted design. The `/media`
watcher does not treat a mounted bundle as an instruction to install, so a
planted USB stick cannot update the device without a command being run. That
satisfies S1 and T-S1 with less code than a marker/`apply` handshake.

### 3.9 LAN / local and HTTPS flow

- **CLI/SSH:** `photo-frame-update install <path>` for a file on the device, or
  `photo-frame-update install <https-url> --sha256 <hex>` over the network. The
  download is verified identically; `--sha256` is required for URL installs and
  pins the expected digest out of band.
- Non-HTTPS URLs are refused, and so is an HTTPS-to-HTTP redirect.
- The config-mode upload page was **not** adopted. Updates run from a shell, over
  the VPN, or from a mounted drive.

### 3.10 Online checks

There is no update-check timer and no background poll. The owner runs `install`
with a known URL, or installs from a local file. This drops the optional
`photo-frame-update check` endpoint and its privacy trade-off (T-I5) entirely
rather than shipping it disabled.

### 3.11 State preservation

Option A never touches:

- `/var/lib/photo-frame/photos` (photos, index, cursors, status),
- `/etc/photo-frame/config.toml`,
- `/etc/photo-frame/display.env` (new keys ship as `display.env.example`; the
  live file is never overwritten),
- hostname, Wi-Fi config, SSH host/user keys.

The app must read configs written by older versions (M7): add
`config_version = 1` to `config.toml`, migrate forward on load, and refuse to run
if the file's schema is newer than the binary supports, with a clear panel
message.

### 3.12 Failure modes and recovery

| Failure | Result |
|---------|--------|
| Bad/missing signature | Refuse before install; audit entry |
| Unknown key ID | Refuse; the bundle was signed with a key this build does not trust |
| Rejected URL | Plain HTTP, or an HTTPS-to-HTTP redirect, refusing before download |
| Hash mismatch / truncated bundle | Refuse; the staged copy is discarded |
| Wrong arch | Refuse with a specific message |
| Sequence lower than the floor | Refuse. An equal sequence is allowed only for a new version directory; there is no `--force`. |
| Release directory already exists | Refuse; the same version cannot be installed twice |
| Disk full | Fail with an I/O error; the staged copy is cleaned up and the live install is untouched |
| Power loss mid-copy/extract | Nothing installed; staging is incomplete and ignored |
| Power loss mid-flip | Atomic symlink rename leaves old or new; the guard confirms or reverts |
| New version fails health check | Automatic revert to previous, or removal of `current` when no fallback exists |
| New version cannot start at all | The guard runs after the app units, detects the failed health check, and reverts |
| Revert target missing | The guard clears `pending` and reports failure; `gc` keeps `previous` |
| Corrupt rootfs/boot partition | Out of scope for A; recover by reflashing |

### 3.13 Pros, cons, effort, risk

| | |
|--|--|
| **Pros** | Simple, auditable, no partition changes, fast, offline-capable, preserves state by construction, low risk, no bootloader work. |
| **Cons** | Does not update OS, kernel, firmware, ImageMagick, or OpenSSL. Userspace boot guard is weaker than a hardware/ bootloader fallback. |
| **Effort** | Medium (updater, bundle format, boot guard, UX, CI signing, tests). |
| **Risk** | Low. The blast radius is two user services and a symlink; the photos partition and config are untouched. |

---

## 4. Option B — Full OS A/B image updates (evaluated, not adopted)

### 4.1 Partition layout

The current layout is boot FAT + root ext4 (~900 MB) + photos ext4. A/B needs a
second root and a place for state that both roots share:

| # | Partition | FS | Size | Purpose |
|---|-----------|----|------|---------|
| 1 | boot | FAT32 | ≥ 256 MB | Firmware, `config.txt`, `cmdline.txt`, kernel(s), initramfs, boot env |
| 2 | rootfs A | ext4 (or dm-verity ext4/squashfs) | ~1.2 GB | Active/inactive OS image |
| 3 | rootfs B | ext4 (or dm-verity ext4/squashfs) | ~1.2 GB | The other OS image |
| 4 | state | ext4 | ~512 MB | `/etc/photo-frame`, `/var/lib/photo-frame/update`, audit log, hostname/Wi-Fi/SSH |
| 5 | photos | ext4 | remainder | Unchanged; photos + index + cursors |
| 6 | recovery | ext4 | ~128 MB | Updater + a minimal rescue kernel/initramfs (optional but recommended) |

Cost: two roots plus state is roughly 3 GB before photos, on top of the current
~900 MB root. On a 16 GB card that is acceptable; on an 8 GB card it is tight.
This is a real, owner-visible trade-off.

### 4.2 What gets written and how the slot is selected

An update writes a complete signed OS image into the **inactive** root (or a
squashfs/ext4 image applied to it). Nothing is written to the running root.

Slot selection on this board:

- The VideoCore firmware reads `config.txt` from the boot FAT, loads the kernel
  named there (or `kernel8.img`) and the initramfs, and passes `cmdline.txt`.
- **The Pi Zero 2 W has no EEPROM bootloader** (that is Pi 4/5), so **`tryboot`
  is not available**. There is **no secure boot** on any Pi: the firmware does
  not verify signatures.
- `config.txt` therefore cannot express native A/B fallback. We must implement
  slot selection and fallback ourselves.

### 4.3 Boot integrity: two viable approaches

**Approach (a) — added U-Boot (or equivalent) with boot counter/fallback.**
`config.txt` chain-loads U-Boot (`kernel=u-boot.bin`). U-Boot stores an
environment with redundant copies on the boot FAT and uses `bootcount`,
`bootlimit`, and `altbootcmd` to switch slots when the new one fails to mark
itself good. Combined with FIT images, U-Boot can also verify a signed
kernel+initramfs. This is the only approach that gives true **boot-time**
fallback (recovery even when the new root cannot boot far enough to run code).
It is the standard RAUC/embedded pattern. Cost: a real bootloader to configure
and maintain.

**Approach (b) — userspace slot flip + watchdog + recovery partition.**
The running root writes the other root, flips `config.txt`/`cmdline.txt` to the
other `PARTUUID`, arms a hardware watchdog
(`dtparam=watchdog=on`, systemd `RuntimeWatchdogSec=`), and reboots. A small
always-present recovery kernel/initramfs on the recovery partition checks a boot
counter and flips back after `bootlimit` failed boots. This is simpler than
U-Boot but still needs a recovery kernel, because a root that cannot boot cannot
flip anything itself. It is **not** sufficient on its own without that recovery
kernel.

**Honest conclusion:** robust A/B on a Zero 2 W requires either U-Boot with
`bootcount`/`altbootcmd` or a dedicated recovery kernel. Pure userspace flip +
watchdog handles "services failed" but not "kernel/rootfs does not boot".

### 4.4 dm-verity and read-only root

Serve each root as a read-only image with a dm-verity hash tree; the root hash
is signed and verified before the root is mounted. This makes the rootfs
**tamper-evident**: any modification is detected at boot.

It is **not tamper-proof**. Without a hardware root of trust or verified boot
(signed kernel verified by the firmware), a physical attacker can replace the
kernel, the bootloader, and the root hash together. dm-verity raises the bar
against accidental corruption and casual tampering, and pairs with a writable
state partition and a tmpfs overlay for `/var` and `/tmp`. It costs CPU for
SHA-256 on a single-core-class board, which is acceptable for a photo frame.

### 4.5 Transports

- **Offline USB:** a signed full image (`photo-frame-os-<ver>.img.zst` plus a
  checksum and signature) and a manifest. Same verification gate as Option A,
  applied to an image.
- **Online:** RAUC's hawkBit backend, a Mender server, or a static signed image
  over HTTPS from an owner-controlled host. Larger payloads (hundreds of MB) and
  a stable connection are needed.

### 4.6 State preservation and migration

The state partition (Section 4.1) is **never** part of an OS image, so
`/etc/photo-frame`, the update log, hostname, Wi-Fi, and SSH identity survive.
The updater runs config-schema migration once, idempotently, before the app
starts on the new root. Photos are on their own partition and are untouched.

### 4.7 Failure modes, power-loss safety, recovery

| Failure | Handling |
|---------|----------|
| Writing the inactive root is interrupted | Safe: it is not mounted; retry or discard |
| Power loss while flipping the slot | **Avoid rewriting `config.txt`.** Keep it constant and load a bootloader/recovery kernel that reads a slot marker stored with redundant copies (U-Boot env) — FAT `config.txt` writes are not atomic or journaled |
| New root fails to boot | U-Boot `bootcount`/`altbootcmd` or the recovery kernel flips back |
| New root boots but services fail | Watchdog + boot counter trigger fallback on reboot |
| Image hash/signature invalid | Refuse before writing |
| Both slots bad | Recovery partition boots a minimal rescue for reflash/repair |

The power-loss story is the strongest argument for U-Boot: a single FAT file
write to `config.txt` is the weak point, and redundant bootloader environments
avoid it.

### 4.8 Existing frameworks (brief evaluation)

| Framework | Fit | Why / why not |
|-----------|-----|---------------|
| **RAUC** | Best fit for Option B | Mature A/B, signed `.raucb` bundles, integrates with U-Boot `bootchooser`/bootcount, supports dm-verity and a state partition. Choose this over hand-rolling. |
| **Mender** | Overkill here | Full OTA server + client, signed artifacts, rollback via U-Boot. Heavy (Go client, server/hosted), assumes connectivity and fleet management. Good for many devices, not one. |
| **SWUpdate** | Viable, more config | Signed images, A/B, many handlers, common in embedded. Needs bootloader support for fallback and more complex configuration than RAUC. |
| **OSTree** | Not recommended | Atomic signed commits and rollback, but it is an atomic-filesystem approach (Fedora IoT-style), heavier for a minimal DietPi image, and still needs a bootloader to select deployments on a Pi. |

**Why not for Option A:** Option A needs no bootloader work at all. A
signed tarball plus a symlink flip is simpler, smaller, and easier to audit than
any of these frameworks. None of them is in the adopted design; RAUC would only
be considered if Option B were ever built.

### 4.9 Pros, cons, effort, risk

| | |
|--|--|
| **Pros** | Updates the whole system (kernel, firmware, libc, OpenSSL, ImageMagick); fixes OS CVEs; enables dm-verity; closest to appliance-grade. |
| **Cons** | Large complexity; partition rework; ~3 GB overhead; needs U-Boot or a recovery kernel; SD write amplification; still not tamper-proof (no hardware root of trust). |
| **Effort** | Large. |
| **Risk** | High. A mistake can make the device unbootable and cost state. |
| **Decision** | Not adopted. OS security comes from apt; kernel and firmware are refreshed manually twice a year. |

---

## 5. Option C — Hybrid staged approach (evaluated, not adopted)

Ship Option A now. Keep Option B possible later by making a few structural
choices today. Do **not** build the A/B machinery until the owner decides OS
updates are required.

This section is the staging proposal that was on the table. The adopted design
takes Option A and skips the Option B reservations: there are no partition
changes, no 1.2 GB reserve, and no attempt to keep a future bootloader
compatible. The final record is Section 15.

### 5.1 Do now

| Task | Why |
|------|-----|
| Sign release artifacts and publish checksums with signatures | Prerequisite for any update (M1, M2, N5). |
| Add `config_version` to `config.toml` + migration | Enables forward-compatible config changes (M7). |
| Add the versioned install layout (`/opt/photo-frame/releases`, `current`) | Makes the flip possible without a partition change. |
| Add `/var/lib/photo-frame/update/state.json` (current, previous, last_good, pending, sequence) and the audit log | Update state and audit (M6, S4). |
| Add `photo-frame-update-guard.service` | Rollback safety (M4). |
| Health check on both units and both sockets after restart | Health signal (S7). |
| Keep `display.env`/`config.toml` as conffiles; ship defaults as `.example` | State preservation (M7). |
| Sign releases in a manual CI workflow with its own protected environment | CI usefulness without the key in the build (S5, N5). |
| Phase 0 hardening (Section 12) | The update mechanism should not rest on unhardened primitives (T-T3, T-T5, T-E4, T-D5, T-D6). |

### 5.2 Interfaces and reservations

These were considered and then dropped. There is no plan to reserve partitions
for a future rootfs B, because Option B is not being built:

- **Boot FAT ≥ 256 MB** and **~1.2 GB of unallocated space** were for a future
  A/B layout. Not adopted.
- **Keeping state outside the rootfs** still happens in the narrow sense that
  the updater never touches `/etc/photo-frame` or the photos partition, but there
  is no separate state partition.
- **Boot-marker contract:** the only marker is the `pending` object in
  `/var/lib/photo-frame/update/state.json`, read by the userspace guard.
- **Bundle schema:** `format` is versioned so a future OS image could be
  distinguished, but no OS image format is defined.

### 5.3 Later (only if OS updates are needed)

Not scheduled. If the decision is ever revisited, this is the shape of the work:

- Add U-Boot with `bootcount`/`altbootcmd` (or a recovery kernel).
- Adopt RAUC for signed `.raucb` OS bundles and bootchooser integration.
- Introduce dm-verity read-only roots plus the state partition.
- Migrate `/etc/photo-frame` and identity files to the state partition.

### 5.4 Why this ordering

- Option A delivers real value (bug fixes, features, safe rollback) with low
  risk and no partition work.
- Option B's cost is only worth paying if OS image updates become a real
  requirement. They have not.
- If the owner never needs OS updates, nothing in Option B was built and no
  complexity was paid.

---

## 6. Transport matrix

| Transport | Works offline | Required | Privacy impact |
|-----------|---------------|----------|----------------|
| Local file (mounted USB, `scp`, local disk) | Yes | Tarball plus `.sha256` and `.sha256.sig` in the same directory; a shell to run `install` | None. No network. |
| HTTPS URL | No | Release URL, `--sha256` digest, signature sidecars fetched from the same URL | GitHub sees the device IP and download timing; no device ID in the request. |
| Plain HTTP | No | Not supported. Refused. | n/a |
| SD reflash | Yes | A computer + card reader | N/A. **Fallback only**; loses state. Not an update path. |

---

## 7. Signing and key management

### 7.1 Format

**Ed25519 with a raw signature, wrapped in a JSON envelope.** The signature is
made with `openssl pkeyutl -sign -rawin` over the exact bytes of the `.sha256`
sidecar. The envelope names the key:

```json
{"key_id":"2026-10","signature":"<base64url>"}
```

There is no certificate, no expiry, no CA, no keyserver, and no trust path. A GPG
detached signature was considered and rejected: it brings keyring and expiry
semantics the device would have to ignore, for no benefit over a 64-byte raw
signature.

### 7.2 Keys

One signing key, used by one manual workflow.

| Key | Where | Signs | Compromise impact |
|-----|-------|-------|-------------------|
| **Update signing key** (`2026-10`) | GitHub environment secret `release-signing`; a backup offline; never on the device | The `.sha256` file for each release | Forged releases. Rotate and reflash, or use a second trusted key. |
| **Recovery key** (optional, not a separate mode) | Offline only; never in CI | A recovery release that installs a clean key list | Same as above, but it is the way back if the signing key leaks. |

The signing workflow is manual: `workflow_dispatch` with a release-tag input,
the `release-signing` environment, and `contents: write` only in that job. There
is no automatic sign→publish chain. `.github/workflows/build.yml` builds the
`.deb` packages and the arm64 update bundle, then creates or updates the draft
release. Signing the checksums and publishing the draft stay manual. See
`docs/updates.md` for the release steps and the environment setup.

CI signing means **CI is the build-integrity trust anchor**. The signature proves
the artifact came through the release process; it does not prove the process was
not compromised. A compromised build machine, dependency, or merged commit
produces a validly signed malicious artifact. The protected environment and its
required reviewer limit the blast radius and make each signing event auditable,
but they do not make CI trustworthy. This is an accepted risk, stated plainly so
nobody mistakes the signature for a build attestation.

Possible future hardening, not planned now: sign through a cloud KMS or a
PKCS#11 HSM so the key never exists as a file, or run the signing step on a
self-hosted runner the build cannot reach.

### 7.3 Distribution and pinning

- The public keys are **compiled into `photo-frame-update`** as a table keyed by
  key ID, for example `TRUSTED_KEYS` in `src/update/keys.rs`. The field is
  `bytes`.
- The bytes are the 32-byte raw Ed25519 public key, not PEM or DER. Get them with
  `openssl pkey -pubout -outform DER | tail -c 32`.
- No public-key file is installed by apt, no keyring is read at runtime, and
  nothing under `/etc/photo-frame` participates in verification.
- `photo-frame-update` ships inside the release bundle, so the trusted key set
  travels with the app. Adding or removing a key requires rebuilding the tool.
- The `.sha256.sig` envelope carries `key_id`; the updater looks that ID up and
  refuses unknown IDs.
- `photo-frame-update keys` prints key IDs only, one per line. It does not print
  fingerprints; compute the SHA-256 of the raw 32-byte public key on the signing
  machine and compare it there.

### 7.4 Rotation and revocation

Rotation is a sequence of releases, not a single switch:

1. Generate the new key and add its public half to `TRUSTED_KEYS` alongside the
   old one. Rebuild `photo-frame-update`.
2. Ship a release signed by the **old** key that contains the new updater.
3. Let the device install that release, so it now trusts both keys.
4. Replace the CI environment secrets with the new key.
5. Sign subsequent releases with the new key.
6. Ship a later release, signed with the new key, that removes the old entry.
7. Retire the old private key.

The overlap exists because a device can only learn a new key from a release it
already trusts. The overlap must last long enough to install step 2. It should
not last longer than that.

Revocation is the same operation with no overlap: if a key is compromised, a
release signed by it cannot be trusted, so removal cannot go through that key.
Use a second trusted key or reflash.

### 7.5 Key loss and compromise

- **Lost key:** the device keeps running. No new release can be signed until the
  key is restored from backup, a second trusted key is used, or the device is
  reflashed with a new key list.
- **Compromised key:** anything signed after the leak is suspect. Remove the key
  from CI, rotate the environment secrets, and recover with a second trusted key
  or a reflash. There is no online revocation and no expiry date to fall back on.
- **Recovery key (optional):** there is no dedicated recovery mode. A second key
  baked into `TRUSTED_KEYS` and held offline is the mechanism you can use. If it
  is skipped, say so, because the only remaining path is a reflash.
- **No expiry:** a key stays trusted until a release removes it. Expiry would
  create a new failure mode (a device with a wrong clock refusing updates) with
  no matching benefit, since the key can be removed explicitly.

### 7.6 Anti-rollback floor and its limits

- Store a monotonic `sequence` in `/var/lib/photo-frame/update/state.json`
  (root-owned, 0600). Refuse bundles with a sequence lower than the floor. An
  equal sequence is accepted only for a new release directory; there is no
  force override.
- **Limits (documented, per T-E6/T-D3 and assumption A-4):** the floor lives on
  the same SD card. An attacker with physical access can edit it and reinstall
  an older validly signed bundle. A full reflash resets it. The floor therefore
  stops network/MITM downgrades and accidental downgrades, **not** physical
  attacks.
- Do **not** use timestamps for the floor: the device clock is not a trust
  anchor (see `plans/UPDATE_THREAT_MODEL.md` §7, anti-rollback).

---

## 8. Rollback mechanics on this board

### 8.1 Option A, step by step

See Section 3.5 for the install sequence. The rollback-relevant mechanics:

1. The `pending` object in `state.json` records `from`, `to`, `sequence`,
   `key_id`, `source`, and `started_at`; written atomically before the flip.
2. `current` is flipped with a single `rename()` of a symlink — atomic on ext4.
3. Success clears `pending`; failure flips back immediately, or removes `current`
   when no previous release is usable.
4. `photo-frame-update-guard.service` runs **after** the app units on every boot.
   It uses the symlink and `state.current` to decide whether the pending release
   activated, then either confirms it or reverts to `pending.from`.
5. Power loss between flip and confirm is covered by the guard on the next boot.

**What is atomic on ext4:** `rename()` of a file or symlink within a directory,
given `data=ordered` (Debian default) plus a directory `fsync`. Temporary staging
files are never used until renamed.

**What is not atomic on FAT:** a partial write to `config.txt` or any FAT file
(the boot partition). Option A does not write to the boot partition, which is
why it is power-safe without a bootloader.

### 8.2 Option B, boot-time rollback

1. The updater writes the inactive root (safe if interrupted; not mounted).
2. The slot marker is updated with redundant copies (U-Boot env) or, if using
   the recovery-kernel approach, the boot counter is incremented.
3. Reboot. U-Boot `bootcount`/`altbootcmd` (or the recovery kernel) selects the
   slot and falls back after `bootlimit` failed boots.
4. A watchdog (`RuntimeWatchdogSec=`) reboots if the new root hangs before it can
   mark itself good.
5. On success the app marks the slot good (resets `bootcount`/sets `boot_ok`).

Power-loss safety here depends on **not** rewriting `config.txt`: keep it
constant and store the slot in the redundant bootloader environment or have the
recovery kernel read the marker. A partial FAT write to `config.txt` is the one
step that can brick the device, so the design must avoid it.

---

## 9. State and config preservation

| Item | Location | Option A | Option B |
|------|----------|----------|----------|
| Photos | `/var/lib/photo-frame/photos` (own partition) | Preserved (untouched) | Preserved (own partition) |
| Index, cursors, status | Photos partition | Preserved | Preserved |
| `config.toml` | `/etc/photo-frame/config.toml` | Preserved (never overwritten) | Preserved via state partition |
| `display.env` | `/etc/photo-frame/display.env` | Preserved (conffile; new keys as `.example`) | Preserved via state partition |
| Update state + audit | `/var/lib/photo-frame/update/state.json`, `/var/log/photo-frame/update.log` | Preserved | Preserved via state partition |
| Hostname | `/etc/hostname` | Preserved (rootfs untouched) | Preserved via state partition / re-applied |
| Wi-Fi | `/etc/wpa_supplicant` (or DietPi config) | Preserved | Preserved via state partition |
| SSH identity | `/etc/ssh`, `authorized_keys` | Preserved | Preserved via state partition |

**Config schema versioning (M7):** add `config_version = 1` to `config.toml`.
On load: if the file version is older, migrate forward and rewrite atomically; if
newer than the binary supports, refuse to run with a clear panel/log message.
Migration functions must be idempotent and covered by tests.

---

## 10. Update UX and audit

### 10.1 Triggering on a headless device

| Method | How | Approval |
|--------|-----|----------|
| CLI/SSH | `photo-frame-update install <path\|url> [--sha256 <hex>]` | Owner runs it |
| Local file | Same command with a path on a mounted drive | Owner runs it |
| Remote shell | Same command over the VPN | Owner runs it, after VPN and SSH auth |

There is no panel-driven approval prompt, no config-mode upload page, and no
background check. A valid bundle sitting on a mounted drive is not an
instruction to install it.

### 10.2 Command output

The updater reports on stdout, and failures on stderr. There is no panel or admin
API integration: no `last-result.json`, no `/api/status` field, and no control
socket messages from the update path.

```
installed 0.2.0 (sequence 7) signed by 2026-10; previous release 0.1.0
rolled back from 0.2.0 to 0.1.0
error: signature key id '2027-04' is not trusted by this build
```

### 10.3 Failure reporting

- The command's exit status and stderr message.
- The `pending` object in `state.json` while an update is unconfirmed.
- A line in the persistent audit log.

### 10.4 Audit log (S4)

`/var/log/photo-frame/update.log`, root-owned, mode 0600, on persistent storage
(not tmpfs). One line per operation:

```
2026-10-26T18:12:03Z action=install from=0.1.0 to=0.2.0 version=0.2.0 sequence=7 key_id=2026-10 source=https://example/bundle.tar.gz result=success
```

Failures add `detail=...`. URL queries and fragments are stripped. This is
separate from the tmpfs operational logs in `/tmp/photo-frame.log`, which do not
survive a reboot.

---

## 11. Security requirements mapping

The Option A column is the adopted design. Option B and Option C are kept for
comparison only.

| Req | Option A (adopted) | Option B | Option C |
|-----|----------|----------|----------|
| M1 authenticated updates | Satisfied (baked keys, fail closed) | Satisfied (signed image/manifest) | Satisfied |
| M2 whole-artifact integrity | Satisfied (per-artifact hashes) | Satisfied (image hash + dm-verity) | Satisfied |
| M3 no root action before verify | Satisfied | Satisfied | Satisfied |
| M4 crash/power-fail + rollback | Satisfied (health check + boot guard) | Satisfied (U-Boot/recovery kernel) | Satisfied |
| M5 fail-closed | Satisfied | Satisfied | Satisfied |
| M6 anti-rollback floor | Satisfied (sequence; physical limits documented) | Satisfied (same; dm-verity detects tamper) | Satisfied |
| M7 state preservation | Satisfied by construction | Satisfied via state partition | Satisfied |
| M8 least privilege / not LAN-triggerable | Satisfied (root updater invoked from a shell; no listener, no panel trigger) | Satisfied | Satisfied |
| M9 offline-capable | Satisfied (local path with sidecars) | Satisfied (USB image) | Satisfied |
| S1 owner approval | Satisfied (owner runs the command) | Satisfied | Satisfied |
| S2 minimal opt-out checks | Satisfied (no background check exists) | Satisfied | Satisfied |
| S3 signed metadata + sequence | Satisfied | Satisfied | Satisfied |
| S4 persistent audit | Satisfied | Satisfied | Satisfied |
| S5 key rotation/revocation | Satisfied (key set in the binary, release-based rotation) | Satisfied | Satisfied |
| S6 sandboxing/atomicity | Satisfied on ext4; no FAT writes | Satisfied if `config.txt` is not rewritten | Satisfied |
| S7 recovery tooling | Satisfied (runbook + previous release kept) | Satisfied (recovery partition) | Satisfied |
| N1 reproducible builds/hashes | Optional (Phase 0) | Optional | Optional |
| N2 dm-verity | Not applicable (no OS image) | Satisfied | Satisfied (Phase 2) |
| N3 encrypted photos | Optional (separate) | Optional (separate) | Optional (separate) |
| N4 rollback command / hold | Satisfied (CLI) | Satisfied | Satisfied |
| N5 sign CI artifacts | Satisfied (dedicated signing job; see 7.2 for the residual risk) | Satisfied | Satisfied |

Option A fails none of the MUST requirements. Its only material gap versus
Option B is that it cannot patch the OS/kernel/dependencies (not a MUST) and
that its rollback is userspace rather than bootloader-level (M4 is still met
because the kernel is unchanged, so the boot guard always runs).

---

## 12. Roadmap

### Phase 0 — Current-state hardening (prerequisite)

These are the findings the update mechanism must not rest on (T-T3, T-T5,
T-E1, T-E4, T-D5, T-D6).

| Task | Finding |
|------|---------|
| Publish signed release artifacts (checksum + signature); sign in a dedicated CI job with a protected environment | T-S1, T-E1, T-E2, N5 |
| Systemd sandboxing: `ProtectSystem=strict`, `ProtectHome=yes`, `PrivateTmp=yes`, `NoNewPrivileges=yes`, `MemoryDenyWriteExecute=yes`, `RestrictAddressFamilies=`, `SystemCallFilter=@system-service`, and explicit `ReadWritePaths=` for the photos dir, `/run/photo-frame`, `/etc/photo-frame` | T-E4 |
| Admin API hardening: constant-time credential comparison (replace `==` in `check_auth`), CSRF protection (Origin/Referer check), request-size limits, and a simple rate limit/lockout | T-D5, T-D6 |
| ImageMagick sandboxing: a hardened `policy.xml` (disable Ghostscript, MVG/MSL, SVG, PDF, and unused coders; set resource limits), run under a restricted profile/namespace | T-T3, T-E3 |
| Fix `token_uri` SSRF: ignore `token_uri` for service accounts or restrict it to the Google token host; reject private/loopback/link-local targets and `http://` | T-T5 |
| Harden `/tmp` files: `O_NOFOLLOW`/ownership checks on the PID lock and setup file | T-T6 |
| Add `config_version` and migration | M7 |
| Add the versioned release layout (`/opt/photo-frame/releases`, `current`) | Phase 1 prerequisite |
| Publish artifact hashes/SBOM and move toward reproducible builds | N1 |
| SSH hardening and firewall policy (VPN/loopback only) | T-E4, M8 |

### Phase 1 — Option A app updates (implemented)

| Task | Output |
|------|--------|
| P1.1 Bundle format + manifest + packaging | `photo-frame-<version>-<arch>.tar.gz` + `.sha256` + `.sha256.sig` |
| P1.2 `photo-frame-update` binary: verify, extract, flip, health check, revert, gc | Root updater |
| P1.3 `photo-frame-update-guard.service` | Userspace rollback after the app units |
| P1.4 Health check on both units and both sockets | Health signal |
| P1.5 Local-path install (mounted drive or `scp`) | Offline install |
| P1.6 HTTPS install with required `--sha256`, HTTP and downgrade redirects refused | Network install |
| P1.7 Baked key list + `keys` command | Trust anchor (S5) |
| P1.8 Command output and errors on stdout/stderr | UX (M4, S4) |
| P1.9 Audit log in `/var/log/photo-frame/update.log` | S4 |
| P1.10 Key rotation procedure; optional second trusted key | S5 |
| P1.11 Manual signing workflow + `release-signing` environment | N5, S5 |
| P1.12 Tests: signature failure, unknown key ID, hash mismatch, traversal, power-loss simulation, revert, wrong arch, downgrade refusal, HTTP refusal | M1–M9 |
| P1.13 Docs: owner update runbook, key custody, OS/kernel policy, VPN access | Docs |
| P1.14 Owner-initiated rollback (`photo-frame-update rollback`) | N4 |

### Phase 2 — Option B OS image A/B

Not scheduled. Kept here only so the analysis is not lost.

| Task | Output |
|------|--------|
| P2.1 Finalize partition plan; move state to a state partition | Layout |
| P2.2 Add U-Boot with `bootcount`/`altbootcmd` (or a recovery kernel) | Boot fallback |
| P2.3 Adopt RAUC; produce signed `.raucb` images | OS updates |
| P2.4 dm-verity read-only roots + writable state/tmpfs overlay | N2 |
| P2.5 Watchdog + `boot_ok` marking | M4, S7 |
| P2.6 Migration of `/etc/photo-frame` and identity files | M7 |
| P2.7 Recovery partition + rescue docs | Recovery |

---

## 13. Open questions (answered)

1. **Is app-only updating enough, or must the OS/kernel/ImageMagick be
   patchable?** App-only. OS and kernel fixes come from `unattended-upgrades`
   and a manual kernel refresh about twice a year.
2. **Signing tool:** OpenSSL Ed25519 with a raw signature over the `.sha256`
   sidecar. Not minisign, not GPG.
3. **Hardware token or offline machine?** Neither. The key lives in a protected
   GitHub environment; a KMS/HSM is a possible later step.
4. **Where do releases live?** GitHub Releases, with the tarball and both
   sidecars attached.
5. **Online check?** No. There is no check timer and no endpoint.
6. **Approval UX:** a shell command, locally or over the VPN. No `apply` marker
   and no config-mode upload page.
7. **Partition reservations?** Not taken. No A/B layout is planned.
8. **State relocation?** Not needed without Option B.
9. **Release channels:** one channel. No stable/beta split.
10. **How much automation?** None for app updates: the owner runs `install`.
    OS security updates are automatic through apt, with no auto-reboot.

---

## 14. Comparison table

| Criterion | Option A (app bundles, adopted) | Option B (OS A/B) | Option C (hybrid) |
|-----------|------------------------|-------------------|-------------------------------|
| Security | Strong for app code; no OS CVE coverage | Strongest; enables dm-verity and signed kernel | Strong now, extensible later |
| Complexity | Low–Medium | High (bootloader, partitions, RAUC) | Medium now, High later if Phase 2 |
| Rollback | Health check + userspace boot guard (reliable for app updates) | Bootloader/recovery-kernel fallback (reliable at boot) | A now, B later |
| Offline | Yes (local file with sidecars) | Yes (USB image) | Yes |
| Effort | Medium | Large | Medium + optional Large |
| State preservation | By construction | Via state partition | By construction now |
| OS/kernel updates | No (apt and manual kernel refresh instead) | Yes | No, until Phase 2 |
| Partition changes | None | Required (~3 GB) | None now; reserved for later |
| Power-loss safety | Strong (no FAT writes) | Strong only if `config.txt` is not rewritten | Strong |
| Risk | Low | High | Low now |

---

## 15. Decision record and operating policy

### 15.1 Decisions

- **App-only signed release bundles on DietPi.** The OS image is managed by
  apt, not by the updater.
- **No A/B partitioning, no RAUC, no SWUpdate, no `dietpi-backup`.** Rollback is
  a symlink flip plus a userspace boot guard, which is sufficient because the
  kernel never changes during an app update.
- **`photo-frame-update` CLI** with versioned releases under
  `/opt/photo-frame/releases/<version>`, a `current` symlink, retention of the
  current, previous, and four most recent releases, a health check, automatic
  rollback, a boot guard, and an audit log.
- **Bundle format:** `photo-frame-<version>-<arch>.tar.gz` with `manifest.json`
  (`format`, `version`, `sequence`, `arch`, `min_version`, `files[]`) and the
  three binaries, plus `.sha256` and `.sha256.sig` sidecars. The signature is
  Ed25519 over the checksum bytes, in a JSON envelope naming the key ID. No
  certificate, no expiry, no CA, no network.
- **Public keys are compiled into `photo-frame-update`** as `TrustedKey { id,
  bytes }` entries in `src/update/keys.rs`. The list starts empty and refuses
  every install until a key is added and the tool is rebuilt. No key file is
  installed by apt and nothing in `/etc` participates in verification. The tool
  ships inside the release bundle.
- **Transports:** a local path (sidecars required in the same directory, optional
  `--sha256`) and an HTTPS URL (required `--sha256`; plain HTTP and HTTPS-to-HTTP
  redirects refused).
- **CI build and signing:** `.github/workflows/build.yml` builds the `.deb`
  packages and the arm64 update bundle, then creates or updates the draft
  release. `.github/workflows/sign.yml` is manual (`workflow_dispatch` with a
  release tag), runs in the `release-signing` environment, signs every
  `*.sha256` asset with `scripts/sign-checksum.sh`, and uploads the
  `*.sha256.sig` files. The sign job has `contents: write`. There is no automatic
  sign→publish chain. CI is the build-integrity trust anchor, and a compromised
  build or merged commit can produce a validly signed malicious artifact; the
  protected environment limits the blast radius but does not remove that risk.
  KMS, an HSM, or a self-hosted signing runner is a possible later hardening
  step.
- **Rotation** adds a key to the updater and rebuilds it, ships that build in a
  release signed by the old key, keeps both during an overlap, then removes the
  old key in a later release signed by the new key. No expiry. Key loss leaves
  the device running but unable to accept a new release until recovery or
  reflash. A compromised key cannot be rotated through itself; use a second
  trusted key or reflash.
- **Releases are published on GitHub Releases.** The device does not poll, and
  there is no update check timer.

### 15.2 OS security updates

- Automatic Debian security updates through `unattended-upgrades`.
- `needrestart` for services that link a patched library.
- No custom maintenance timer and no self-written apt wrapper.
- Kernel, firmware, and bootloader packages are held.
- Manual kernel/firmware/bootloader update roughly twice a year over the VPN,
  with a known-good SD card on hand.
- No automatic reboot.

### 15.3 Remote access

- Mesh VPN (Tailscale, self-hosted Headscale, or NetBird). No port forwarding.
- SSH on the VPN and loopback only; the firewall drops LAN SSH.
- Key-only authentication, no root login, a dedicated `admin` user, and `sudo`
  scoped to `photo-frame-update`.
- Config mode's admin server stays LAN-only and is not exposed over the VPN.
- Remote update workflow: copy the bundle over `scp`, run
  `photo-frame-update install` locally, check `status` and the audit log.

See `docs/updates.md` for the commands, key ceremony, CI workflow, rotation
steps, and troubleshooting.

---

*End of update strategy.*
