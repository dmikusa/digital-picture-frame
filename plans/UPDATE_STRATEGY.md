# Photo Frame — Device Update Strategy

## How to use this document

This is a planning document, not a specification. It defines three concrete
update plans, compares them, and recommends a staged path. It is written to be
turned into issues: each phase and task below can become a GitHub issue with the
IDs given.

It is a **living document**. Revise it when a phase lands, when the threat model
(`plans/UPDATE_THREAT_MODEL.md`) changes, or when the owner answers an open
question. Keep the comparison table and the requirements mapping current.

Companion: `plans/UPDATE_THREAT_MODEL.md` holds the findings (`T-nn`) and the
MUST/SHOULD/NICE requirements (`M`/`S`/`N`) referenced throughout.

---

## 1. Executive summary

The frame currently updates by reflashing the SD card. That is not an update
path: it wipes `config.toml` (and its cloud credentials), the index, cursors,
hostname, Wi-Fi, and SSH identity, and it requires a computer and physical
access. The device also has no hardware secure boot, no A/B boot slots, and no
`tryboot`/EEPROM fallback, so any update design must supply its own
authenticity, rollback, and recovery.

The recommended approach is **Option C: ship app-only signed updates first
(Option A), and keep the door open for full OS image updates later (Option B)**
by reserving partitions and putting state outside the rootfs now. Option A:

- updates the two binaries, systemd units, and docs — not the OS or kernel;
- installs from a signed bundle verified against an **owner-pinned public key**
  before anything privileged happens;
- uses versioned install directories plus an atomic `current` symlink flip;
- health-checks the new version and **automatically reverts** on failure, with a
  userspace boot guard that covers power loss and a new version that will not
  start;
- works fully **offline from USB**, over the **LAN**, or via an **opt-in**
  online check;
- never turns the frame into a permanently listening service; and
- preserves all state by construction, because it never touches the photos
  partition or `/etc/photo-frame`.

Option B (full OS A/B images) is the only way to patch the kernel, ImageMagick,
OpenSSL, and the OS. It is large, changes the partition layout, and on this
board still needs a bootloader (U-Boot) or a recovery kernel to make rollback
work at boot. It should be built only if the owner decides OS updates are
actually needed.

**The key decision for the owner:** is app-only updating sufficient, or must the
device be able to update the OS and kernel? If app-only is sufficient, build
Option A now and stop. If OS updates are required, commit to Option C and accept
the partition and bootloader work.

---

## 2. Requirements

Distilled from `plans/UPDATE_THREAT_MODEL.md`. These are the acceptance criteria
for any option; Section 12 maps each option against them.

### Must

| ID | Requirement |
|----|-------------|
| M1 | Only owner-signed bundles install; verification fails closed. |
| M2 | Every artifact's hash is verified against the signed manifest before use. |
| M3 | A complete update installs from USB with no network. |
| M4 | Photos, `config.toml`, `display.env`, index/cursors/status, and hostname/Wi-Fi/SSH identity survive. |
| M5 | Two-phase install with automatic revert on failure; power loss is safe. |
| M6 | No always-on listener; online checks are opt-in. |
| M7 | Persistent, root-owned audit log of updates, separate from tmpfs logs. |
| M8 | Monotonic sequence floor for anti-rollback, with limitations documented. |
| M9 | Signature, product, arch, sequence, and hashes verified before any root action. |
| M10 | Bundle extraction rejects traversal, symlinks, hardlinks, devices, and absolute paths. |

### Should

| ID | Requirement |
|----|-------------|
| S1 | Owner-initiated or owner-approved; no silent auto-update. |
| S2 | Atomic installs; safe at every step against power loss. |
| S3 | Key rotation and revocation without reflashing. |
| S4 | Config schema versioning with forward migration. |
| S5 | Least privilege for the updater and staging area. |
| S6 | CI never holds the root/authority signing key. |
| S7 | Health check covers both services, sockets, and index availability. |
| S8 | Failures visible on the panel and in the audit log. |

### Nice

| ID | Requirement |
|----|-------------|
| N1 | Opt-in online check with a minimal, privacy-preserving request. |
| N2 | Panel notification when an automatic revert happens. |
| N3 | Tamper-evident read-only root (dm-verity) for OS images. |
| N4 | Release channels (stable/beta). |
| N5 | UPS/capacitor for power-loss safety. |

---

## 3. Option A — App-only signed release bundles

### 3.1 What is updated

| Updated | Not updated |
|---------|-------------|
| `photo-frame-manager` (Rust) | OS packages, libc, OpenSSL |
| `photo-frame-display` (C) | Kernel, firmware, boot files |
| systemd units (if changed) | ImageMagick and other runtime deps |
| `photo-frame-update` helper (new) | Partition layout |
| docs / `display.env` **defaults** (never the live file) | `config.toml` |

This is deliberately narrow. It fixes application bugs and ships features. It
cannot fix a kernel CVE, an ImageMagick CVE, or an OpenSSL CVE — those require
Option B.

### 3.2 Signed bundle format

One file, plus a detached signature. Keep it simple and auditable.

```
photo-frame-0.2.0-0007.pfupdate          # gzip-compressed tar
photo-frame-0.2.0-0007.pfupdate.minisig  # detached Ed25519 signature
```

The tar contains a manifest at the root and a `payload/` tree:

```
manifest.json
payload/usr/bin/photo-frame-manager
payload/usr/bin/photo-frame-display
payload/usr/bin/photo-frame-update
payload/lib/systemd/system/photo-frame-manager.service
payload/lib/systemd/system/photo-frame-display.service
payload/lib/systemd/system/photo-frame-boot-guard.service
payload/etc/photo-frame/display.env.example
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
  "created": "2026-10-21T00:00:00Z",
  "key_id": "owner-2026-a",
  "artifacts": [
    {"path": "usr/bin/photo-frame-manager", "sha256": "…", "size": 4123456, "mode": "0755"},
    {"path": "usr/bin/photo-frame-display", "sha256": "…", "size": 51200, "mode": "0755"},
    {"path": "lib/systemd/system/photo-frame-manager.service", "sha256": "…", "size": 512, "mode": "0644"}
  ]
}
```

| Field | Purpose |
|-------|---------|
| `bundle_format` | Refuse formats the updater does not understand. |
| `product`, `arch` | Refuse bundles for another device. |
| `version` | Human-readable semver; used for the install dir name. |
| `sequence` | Monotonic integer; the anti-rollback floor (M8). |
| `min_version` | Refuse if the installed version is older than the bundle supports. |
| `config_schema` | The config schema the app expects; drives migration (S4). |
| `key_id` | Which trusted key signed it; enables rotation (S3). |
| `artifacts[]` | Path, SHA-256, size, mode for every installed file (M2). |

**Why a detached signature over the whole bundle** rather than a signature field
inside the manifest: the signature must cover the manifest *and* the payload. A
detached signature over the exact bundle bytes does that with no ambiguity, and
it lets the device verify before it parses untrusted JSON (M1, M9).

### 3.3 Signing

Recommended: **minisign / Ed25519**, detached, with the public key pinned on the
device.

- Small, no keyring, no expiry or keyserver semantics, one public key to pin.
- The private key is password-protected and kept **offline** (never on the
  device, never in CI).
- A GPG detached signature (`gpg --detach-sign`, Ed25519 subkey) is an
  acceptable alternative, and the owner already uses GPG for commits. If GPG is
  chosen, the device must pin the **exact public key bytes**, not a keyring, and
  must not honor expiry or trust paths.

CI produces unsigned artifacts. The owner signs the bundle (or a release
manifest) on an offline machine. See Section 9 for the two-tier key design that
keeps CI useful without holding the root key.

### 3.4 On-device verification (before any privileged action)

The updater runs as root (it must replace binaries and units) but performs all
of this **before** touching the live install:

1. Copy the bundle to a root-only staging dir
   `/var/lib/photo-frame/update/staging/` (0700), writing to `*.part` and
   renaming when complete. An interrupted copy is never a candidate.
2. Verify the detached signature over the bundle bytes with the pinned key.
   **Fail closed** on a missing/invalid signature (M1).
3. Parse `manifest.json`. Check `bundle_format`, `product`, `arch`,
   `config_schema`, and `min_version` (M9).
4. Compare `sequence` with the stored floor; refuse `<= floor` unless
   `--force` is passed (M8).
5. Verify each `artifacts[]` entry's SHA-256 and size against the payload (M2).
6. Extract the payload into a **fresh** directory, rejecting absolute paths,
   `..`, symlinks, hardlinks, device nodes, and any entry outside the target
   (M10). Set ownership to root and modes from the manifest.
7. Only now proceed to install (Section 3.5).

### 3.5 Versioned installs and the flip

```
/opt/photo-frame/
  versions/
    0.1.0-0000/            # seeded from the original .deb install
      bin/photo-frame-manager
      bin/photo-frame-display
      bin/photo-frame-update
      units/*.service
    0.2.0-0007/            # newly verified
  current -> versions/0.2.0-0007
/var/lib/photo-frame/update/
  sequence                 # monotonic floor (root, 0644)
  pending.json             # {from, to, attempts, started_at}
  confirmed.json           # written after a successful health check
  last-result.json         # for the panel and /api/status
  staging/
  audit.log                # persistent, root-owned, 0600
```

The systemd units are installed once and point at the stable path
`/opt/photo-frame/current/bin/...`. A normal update only flips the symlink, so
unit churn is rare. If a bundle does change a unit, the updater copies it to
`/etc/systemd/system/`, runs `systemctl daemon-reload`, and records it in the
pending marker so the revert restores the previous unit.

Install sequence (all on ext4, all atomic via `rename()`):

1. Verify (Section 3.4).
2. Extract to `versions/<ver>-<seq>.new`; `rename()` to `versions/<ver>-<seq>`.
3. Write `pending.json` (temp + `rename` + `fsync` the directory).
4. Create `current.new` symlink, then `rename()` it over `current` (atomic).
5. `systemctl restart photo-frame-display photo-frame-manager`.
6. Health check (Section 3.6).
7. On success: write `confirmed.json`, remove `pending.json`, append to
   `audit.log`, show "Updated to vX".
8. On failure: revert `current` to the previous version, restart, mark the
   result failed, append to `audit.log`, show "Update failed, reverted to vN".

### 3.6 Health check

Run for up to 90 seconds after restart:

| Check | Pass condition |
|-------|----------------|
| Services | `systemctl is-active` for both units |
| Crash loop | `NRestarts` did not exceed a small threshold |
| Display socket | `/run/photo-frame/photo-frame.sock` exists and is a socket |
| Manager liveness | Manager writes `/run/photo-frame/healthy` with its version and a timestamp (new, small feature) |
| Index | Photos dir is mounted and readable |

The manager writes `/run/photo-frame/healthy` once it is past startup and has
opened the index. That is a tiny, low-risk addition and gives the updater a
definitive liveness signal rather than a sleep.

### 3.7 Boot guard (userspace rollback)

There is no hardware fallback, so recovery must work even if the new version
cannot start at all. `photo-frame-boot-guard.service` is a `oneshot` unit
ordered `Before=photo-frame-display.service photo-frame-manager.service` and
`After=local-fs.target`:

1. If `confirmed.json` exists, clear `pending.json` and exit.
2. If `pending.json` exists and is unconfirmed, increment `attempts`.
3. If `attempts >= 1` (we already booted once without confirmation), revert
   `current` to `pending.from`, restore any replaced units, restart
   `systemd`, and record the revert in `audit.log`.
4. Exit; the normal services start against the good version.

Because the kernel is unchanged, the boot guard always runs. This is what makes
app-only rollback reliable without reflashing. The only unrecoverable case is a
corrupt rootfs or boot partition, which Option A does not touch.

### 3.8 Offline USB flow

1. The owner copies `photo-frame-<ver>-<seq>.pfupdate` and `.minisig` to a
   FAT-formatted USB stick, under a marker directory `photo-frame-update/`.
2. Inserting the stick is detected by the existing `/media` watcher. A
   `photo-frame-update/` directory means **update**, not photo import.
3. The panel shows: "Update v0.2.0 (seq 7) ready. Remove and re-insert with
   `apply` to install." The owner adds an empty `photo-frame-update/apply` file
   (or uses the config-mode button) to approve. Presence of a valid bundle alone
   is **not** approval (S1, T-10).
4. On approval the updater verifies and installs. The panel shows progress and
   the final result.
5. If anything fails, nothing is applied and the panel explains why.

A second physical step (remove/re-insert with `apply`) is intentionally
awkward: it prevents a planted drive from installing silently.

### 3.9 LAN / local flow

- **Config-mode button:** add an "Update" section to the admin UI that accepts an
  uploaded bundle (or a LAN URL) and triggers the same verified install. This
  reuses the existing rotating-password session and disappears with config mode,
  so it adds no permanent exposure (M6).
- **CLI/SSH:** `photo-frame-update install /path/to/bundle` or
  `photo-frame-update install https://host/bundle` for an owner on the LAN. The
  download is verified identically.

### 3.10 Optional online check

- A systemd timer, **disabled by default**, runs
  `photo-frame-update check`.
- The check does one HTTPS `GET` of a static manifest from an owner-controlled
  URL, compares `version`/`sequence`, and if newer shows a panel message and
  writes `last-result.json`. It **does not download or install** (S1, N1).
- Request is minimal: no device ID, no telemetry, a fixed `User-Agent`. Privacy
  impact (T-19) is that the endpoint operator (the owner) sees the device IP and
  poll timing. Document it.
- Installing from the online source requires the same explicit approval and the
  same signature verification as USB.

### 3.11 State preservation

Option A never touches:

- `/var/lib/photo-frame/photos` (photos, index, cursors, status),
- `/etc/photo-frame/config.toml`,
- `/etc/photo-frame/display.env` (new keys ship as `display.env.example`; the
  live file is never overwritten),
- hostname, Wi-Fi config, SSH host/user keys.

The app must read configs written by older versions (S4): add
`config_version = 1` to `config.toml`, migrate forward on load, and refuse to run
if the file's schema is newer than the binary supports, with a clear panel
message.

### 3.12 Failure modes and recovery

| Failure | Result |
|---------|--------|
| Bad/missing signature | Refuse before install; panel error; audit entry |
| Hash mismatch / truncated bundle | Refuse; the `*.part` staging file is discarded |
| Wrong arch/product | Refuse with a specific message |
| Sequence not newer | Refuse unless `--force` |
| Disk full | Refuse after a pre-flight space check; no changes |
| Power loss mid-copy/extract | Nothing installed; staging is incomplete and ignored |
| Power loss mid-flip | Atomic symlink rename leaves old or new; boot guard decides |
| New version fails health check | Automatic revert to previous; panel message (N2) |
| New version cannot start at all | Boot guard reverts on the next boot |
| Revert target missing | Boot guard falls back to the newest confirmed version; the original `.deb` install is seeded as `versions/<base>-0000` |
| Corrupt rootfs/boot partition | Out of scope for A; recover via Option B or reflash |

### 3.13 Pros, cons, effort, risk

| | |
|--|--|
| **Pros** | Simple, auditable, no partition changes, fast, offline-capable, preserves state by construction, low risk, no bootloader work. |
| **Cons** | Does not update OS, kernel, firmware, ImageMagick, or OpenSSL. Userspace boot guard is weaker than a hardware/ bootloader fallback. |
| **Effort** | Medium (updater, bundle format, boot guard, UX, CI signing, tests). |
| **Risk** | Low. The blast radius is two user services and a symlink; the photos partition and config are untouched. |

---

## 4. Option B — Full OS A/B image updates

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

- **Offline USB:** a signed full image (`photo-frame-os-<ver>.img.zst` +
  `.minisig`) plus manifest. Same verification gate as Option A, applied to an
  image.
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
minisign-verified tarball plus a symlink flip is simpler, smaller, and easier to
audit than any of these frameworks. Use RAUC only if and when Option B is built.

### 4.9 Pros, cons, effort, risk

| | |
|--|--|
| **Pros** | Updates the whole system (kernel, firmware, libc, OpenSSL, ImageMagick); fixes OS CVEs; enables dm-verity; closest to appliance-grade. |
| **Cons** | Large complexity; partition rework; ~3 GB overhead; needs U-Boot or a recovery kernel; SD write amplification; still not tamper-proof (no hardware root of trust). |
| **Effort** | Large. |
| **Risk** | High. A mistake can make the device unbootable and cost state. |

---

## 5. Option C — Hybrid staged approach (recommended)

Ship Option A now. Keep Option B possible later by making a few structural
choices today. Do **not** build the A/B machinery until the owner decides OS
updates are required.

### 5.1 Do now

| Task | Why |
|------|-----|
| Sign release artifacts and publish a signed manifest | Prerequisite for any update (M1, M2, S6). |
| Add `config_version` to `config.toml` + migration | Enables forward-compatible config changes (S4). |
| Add the versioned install layout (`/opt/photo-frame/versions`, `current`) and seed `versions/<base>-0000` from the current `.deb` | Makes the flip possible without a partition change. |
| Add `/var/lib/photo-frame/update/` (sequence, pending, confirmed, audit) | Update state and audit (M7, M8). |
| Add `photo-frame-boot-guard.service` | Rollback safety (M5). |
| Add `/run/photo-frame/healthy` from the manager | Health check signal (S7). |
| Keep `display.env`/`config.toml` as conffiles; ship defaults as `.example` | State preservation (M4). |
| Publish a **two-tier** signing setup (offline root key + CI build key) | CI usefulness without the root key (S6). |
| Phase 0 hardening (Section 11) | The update mechanism should not rest on unhardened primitives (T-03/04/05/15). |

### 5.2 Interfaces and reservations to fix now

These are cheap now and expensive later:

- **Boot FAT ≥ 256 MB** so it can hold two kernels/initramfs plus a bootloader
  env.
- **Reserve ~1.2 GB** of unallocated space (or an unused partition) after the
  photos partition for a future rootfs B. Document the partition plan in
  `docs/deployment.md`.
- **Keep state outside the rootfs.** Plan to relocate `/etc/photo-frame` to a
  dedicated state partition (or a bind mount from the photos partition) before
  Option B. Option A already assumes state is not owned by the app image.
- **Define a boot-marker contract** — a documented location/format that both the
  app boot guard and a future bootloader/recovery kernel can read
  (`/var/lib/photo-frame/update/pending.json` + a copy on the boot FAT).
- **Agree on the bundle/ manifest schema** so the same manifest can later
  describe either an app bundle or an OS image (`bundle_format` distinguishes
  them).

### 5.3 Later (only if OS updates are needed)

- Add U-Boot with `bootcount`/`altbootcmd` (or a recovery kernel).
- Adopt RAUC for signed `.raucb` OS bundles and bootchooser integration.
- Introduce dm-verity read-only roots plus the state partition.
- Migrate `/etc/photo-frame` and identity files to the state partition.

### 5.4 Why this ordering

- Option A delivers real value (bug fixes, features, safe rollback) with low
  risk and no partition work.
- The reservations above are the only things Option B truly needs from the
  current layout; deferring them is what would force a disruptive reflash later.
- If the owner never needs OS updates, nothing in Option B was built and no
  complexity was paid.

---

## 6. Transport matrix

| Transport | Works offline | Required | Privacy impact |
|-----------|---------------|----------|----------------|
| Offline USB | Yes | FAT stick, signed bundle + signature, `photo-frame-update/` marker and `apply` file | None. No network. |
| LAN / local (config-mode upload, SSH, `scp`) | Yes (LAN only) | Owner device on the same LAN; existing admin session or SSH; verified bundle | LAN only; credentials cross the LAN as today (T-02). |
| Online check (opt-in) | No | Internet + owner-controlled HTTPS endpoint; timer enabled | Endpoint sees device IP and poll timing; no device ID (T-19). |
| Online install (opt-in) | No | Same as check, plus explicit approval and signature verification | As above; payload downloaded from the owner's endpoint. |
| SD reflash | Yes | A computer + card reader | N/A. **Fallback only**; loses state. Not an update path. |

---

## 7. Signing and key management

### 7.1 Format

Recommended: **minisign/Ed25519 detached signature** over the bundle bytes.
Alternative: **GPG detached signature** with an Ed25519 subkey (the owner
already uses GPG for commits). If GPG is used, pin the exact public key bytes
and ignore expiry/trust paths. Both are acceptable; pick one and document it.

### 7.2 Keys

Two tiers, so CI can build without holding the root key (S6):

| Key | Where | Signs | Compromise impact |
|-----|-------|-------|-------------------|
| **Root/authority key** | Offline machine or hardware token; passphrase-protected; never on the device or in CI | The release manifest (version, sequence, hashes, key list) and key-rotation statements | Forged releases. Highest value; protect most. |
| **Build key** | CI secret | Artifact hashes recorded in the CI-produced manifest | Cannot forge a release without the root key signing the manifest. |

Flow: CI builds and attaches artifacts and a candidate manifest; the owner
downloads and signs the manifest (or a small release file) with the root key,
then publishes the signed manifest next to the artifacts. The device verifies
the manifest with the pinned root key, then verifies artifact hashes from it.
CI's key alone cannot produce an installable release.

If a two-tier flow is too much for one device, the minimal viable version is:
CI builds and attaches **unsigned** artifacts; the owner signs locally before
publishing. CI then holds no signing key at all.

### 7.3 Distribution and pinning

- The public root key is shipped in the first `.deb` and pinned at
  `/etc/photo-frame/update-trusted-keys/<key_id>.pub` (root-owned, 0644).
- The device trusts a **set** of keys keyed by `key_id`, not a keyring and not a
  trust path.
- The manifest carries `key_id`, so a signature is checked against the named
  trusted key.

### 7.4 Rotation and revocation

- **Rotation:** a new key is added by a bundle signed with the old key (a signed
  key-allowlist update), or out-of-band by the owner via SSH/USB (break-glass).
- **Revocation:** remove the key from the trusted set via a statement signed by
  a still-trusted key. If the root key itself is compromised, the only clean
  recovery is a physical/out-of-band key update, because anything signed with
  the old key must be assumed hostile.
- Record the active `key_id` in `last-result.json` and the audit log.

### 7.5 Anti-rollback floor and its limits

- Store a monotonic `sequence` in `/var/lib/photo-frame/update/sequence`
  (root-owned). Refuse bundles with `sequence <= floor` unless explicitly forced.
- **Limits (documented, per T-08/T-16/A1):** the floor lives on the same SD
  card. An attacker with physical access can edit it and reinstall an older
  validly signed bundle. A full reflash resets it. The floor therefore stops
  network/MITM downgrades and accidental downgrades, **not** physical attacks.
- Do **not** use timestamps for the floor: there is no RTC and the clock can be
  wrong (A2, T-16).

---

## 8. Rollback mechanics on this board

### 8.1 Option A, step by step

See Section 3.5 for the install sequence. The rollback-relevant mechanics:

1. `pending.json` records `from`, `to`, and `attempts`; written atomically.
2. `current` is flipped with a single `rename()` of a symlink — atomic on ext4.
3. Success writes `confirmed.json`; failure flips back immediately.
4. `photo-frame-boot-guard.service` runs before the app units on every boot and
   reverts if `pending` is unconfirmed and `attempts >= 1`.
5. Power loss between flip and confirm is covered by the boot guard.

**What is atomic on ext4:** `rename()` of a file or symlink within a directory,
given `data=ordered` (Debian default) plus a directory `fsync`. Writes to a
`*.part` file are not atomic but are never used until renamed.

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
| Update state + audit | `/var/lib/photo-frame/update/` | Preserved | Preserved via state partition |
| Hostname | `/etc/hostname` | Preserved (rootfs untouched) | Preserved via state partition / re-applied |
| Wi-Fi | `/etc/wpa_supplicant` (or DietPi config) | Preserved | Preserved via state partition |
| SSH identity | `/etc/ssh`, `authorized_keys` | Preserved | Preserved via state partition |

**Config schema versioning (S4):** add `config_version = 1` to `config.toml`.
On load: if the file version is older, migrate forward and rewrite atomically; if
newer than the binary supports, refuse to run with a clear panel/log message.
Migration functions must be idempotent and covered by tests.

---

## 10. Update UX and audit

### 10.1 Triggering on a headless device

| Method | How | Approval |
|--------|-----|----------|
| USB marker | `photo-frame-update/` dir with a signed bundle; then add `apply` | Explicit second physical step (S1, T-10) |
| Config-mode button | Upload/URL in the admin UI | Owner is in the authenticated session |
| CLI/SSH | `photo-frame-update install <path|url>` | Owner runs it |
| Online check | `photo-frame-update check` or an opt-in timer | Notification only; install still needs approval |

### 10.2 Panel messages

- "Update vX (seq N) ready — add `apply` to install."
- "Verifying update…"
- "Applying update… the frame may restart."
- "Updated to vX."
- "Update failed: <reason>. Reverted to vN." (N2)
- "Update rejected: bad signature." / "wrong device" / "not newer".

These use the existing control socket (`CLR`/`SHOW`) and status-screen rendering
in `src/qr.rs` / `src/config_mode.rs`, so no new display protocol is needed.

### 10.3 Failure reporting

- Panel message (above).
- `last-result.json` in the update state dir, surfaced in config-mode
  `/api/status` alongside sync status.
- A line in the persistent audit log.

### 10.4 Audit log (M7)

`/var/lib/photo-frame/update/audit.log`, root-owned, mode 0600, on persistent
storage (not tmpfs). One line per attempt:

```
2026-10-21T14:03:11Z action=install from=0.1.0-0000 to=0.2.0-0007 seq=7
  bundle_sha256=… key_id=owner-2026-a trigger=usb result=success
```

Rotate at a small cap (for example 1 MB, keep 3). This is separate from the
tmpfs operational logs in `/tmp/photo-frame.log`, which do not survive a reboot.

---

## 11. Security requirements mapping

| Req | Option A | Option B | Option C |
|-----|----------|----------|----------|
| M1 authenticity | Satisfied (pinned key, fail closed) | Satisfied (signed image/manifest) | Satisfied |
| M2 integrity | Satisfied (per-artifact hashes) | Satisfied (image hash + dm-verity) | Satisfied |
| M3 offline | Satisfied (USB) | Satisfied (USB image) | Satisfied |
| M4 state preservation | Satisfied by construction | Satisfied via state partition | Satisfied |
| M5 rollback | Satisfied (health check + boot guard) | Satisfied (U-Boot/recovery kernel) | Satisfied |
| M6 no permanent exposure | Satisfied (config mode only; opt-in check) | Satisfied | Satisfied |
| M7 audit log | Satisfied | Satisfied | Satisfied |
| M8 anti-rollback floor | Satisfied (sequence; physical limits documented) | Satisfied (same; dm-verity detects tamper) | Satisfied |
| M9 verify before privilege | Satisfied | Satisfied | Satisfied |
| M10 safe extraction | Satisfied | Satisfied (image apply) | Satisfied |
| S1 owner approval | Satisfied (explicit step) | Satisfied | Satisfied |
| S2 atomicity | Satisfied on ext4; no FAT writes | Satisfied if `config.txt` is not rewritten | Satisfied |
| S3 rotation/revocation | Satisfied (key set + signed statements) | Satisfied | Satisfied |
| S4 config schema | Satisfied (`config_version`) | Satisfied | Satisfied |
| S5 least privilege | Satisfied (root updater, narrow staging) | Satisfied | Satisfied |
| S6 CI without root key | Satisfied (two-tier) | Satisfied | Satisfied |
| S7 health check | Satisfied | Satisfied (watchdog + boot_ok) | Satisfied |
| S8 visible failures | Satisfied | Satisfied | Satisfied |
| N1 opt-in online | Satisfied | Satisfied | Satisfied |
| N2 revert notice | Satisfied | Satisfied | Satisfied |
| N3 dm-verity | Not applicable (no OS image) | Satisfied | Satisfied (Phase 2) |
| N4 channels | Optional | Optional | Optional |
| N5 UPS | N/A | N/A | N/A |

Option A fails none of the MUST requirements. Its only material gap versus
Option B is that it cannot patch the OS/kernel/dependencies (not a MUST) and
that its rollback is userspace rather than bootloader-level (M5 is still met
because the kernel is unchanged).

---

## 12. Recommended roadmap

### Phase 0 — Current-state hardening (prerequisite)

These are the findings the update mechanism must not rest on (T-01…T-06,
T-15).

| Task | Finding |
|------|---------|
| Publish signed release artifacts + a signed manifest; adopt the two-tier key split | T-06, S6 |
| Systemd sandboxing: `ProtectSystem=strict`, `ProtectHome=yes`, `PrivateTmp=yes`, `NoNewPrivileges=yes`, `MemoryDenyWriteExecute=yes`, `RestrictAddressFamilies=`, `SystemCallFilter=@system-service`, and explicit `ReadWritePaths=` for the photos dir, `/run/photo-frame`, `/etc/photo-frame` | T-15 |
| Admin API hardening: constant-time credential comparison (replace `==` in `check_auth`), request-size limits, and a simple rate limit/lockout | T-01 |
| ImageMagick sandboxing: a hardened `policy.xml` (disable Ghostscript, MVG/MSL, SVG, PDF, and unused coders; set resource limits), run under a restricted profile/namespace | T-04, T-14 |
| Fix `token_uri` SSRF: ignore `token_uri` for service accounts or restrict it to the Google token host; reject private/loopback/link-local targets and `http://` | T-05 |
| Add `config_version` and migration | S4 |
| Add the versioned install layout and seed `versions/<base>-0000` | Phase 1 prerequisite |

### Phase 1 — Option A app updates

| Task | Output |
|------|--------|
| P1.1 Bundle format + manifest + signing tooling | `photo-frame-0.x.y-NNNN.pfupdate` + `.minisig` |
| P1.2 `photo-frame-update` binary: verify, extract, flip, health check, revert | Root updater |
| P1.3 `photo-frame-boot-guard.service` | Userspace rollback |
| P1.4 Manager writes `/run/photo-frame/healthy` | Health signal |
| P1.5 USB marker + `apply` flow via the `/media` watcher | Offline install |
| P1.6 Config-mode "Update" page (upload/URL) | LAN install |
| P1.7 Optional `photo-frame-update check` + disabled timer | Online check (N1) |
| P1.8 Panel messages via the control socket | UX (S8, N2) |
| P1.9 Audit log + `last-result.json` + `/api/status` surfacing | M7, S8 |
| P1.10 Key pinning, `key_id`, rotation/revocation statements | S3 |
| P1.11 Tests: signature failure, hash mismatch, traversal, power-loss simulation, revert, wrong arch, downgrade refusal | M1–M10 |
| P1.12 Docs: owner update runbook, key custody, USB layout | Docs |

### Phase 2 — Option B OS image A/B (only if needed)

| Task | Output |
|------|--------|
| P2.1 Finalize partition plan; move state to a state partition | Layout |
| P2.2 Add U-Boot with `bootcount`/`altbootcmd` (or a recovery kernel) | Boot fallback |
| P2.3 Adopt RAUC; produce signed `.raucb` images | OS updates |
| P2.4 dm-verity read-only roots + writable state/tmpfs overlay | N3 |
| P2.5 Watchdog + `boot_ok` marking | S7 |
| P2.6 Migration of `/etc/photo-frame` and identity files | M4 |
| P2.7 Recovery partition + rescue docs | Recovery |

---

## 13. Open questions for the owner

1. **Is app-only updating enough, or must the OS/kernel/ImageMagick be
   patchable?** This is the decision that determines whether Option B is ever
   built.
2. **Signing tool:** minisign, or GPG detached signatures (already used for
   commits)?
3. **Hardware token** for the offline root key, or a passphrase-protected key on
   an offline machine?
4. **Where do release artifacts and the signed manifest live?** GitHub Releases,
   or the owner's own server?
5. **Online check:** wanted at all? If yes, which endpoint, and is the privacy
   trade-off (IP + timing) acceptable?
6. **Approval UX on a headless device:** USB `apply` marker, config-mode button,
   SSH, or more than one?
7. **Partition reservations:** acceptable to reserve ~1.2 GB for a future rootfs
   B and grow the boot FAT to ≥ 256 MB?
8. **State relocation:** acceptable to move `/etc/photo-frame` to a dedicated
   state partition before Option B?
9. **Release channels:** is a stable/beta split wanted (N4)?
10. **How much update automation is acceptable**, given that silent
    auto-update is undesirable and owner approval is required (S1)?

---

## 14. Comparison table

| Criterion | Option A (app bundles) | Option B (OS A/B) | Option C (hybrid, recommended) |
|-----------|------------------------|-------------------|-------------------------------|
| Security | Strong for app code; no OS CVE coverage | Strongest; enables dm-verity and signed kernel | Strong now, extensible later |
| Complexity | Low–Medium | High (bootloader, partitions, RAUC) | Medium now, High later if Phase 2 |
| Rollback | Health check + userspace boot guard (reliable for app updates) | Bootloader/recovery-kernel fallback (reliable at boot) | A now, B later |
| Offline | Yes (USB) | Yes (USB image) | Yes |
| Effort | Medium | Large | Medium + optional Large |
| State preservation | By construction | Via state partition | By construction now |
| OS/kernel updates | No | Yes | No, until Phase 2 |
| Partition changes | None | Required (~3 GB) | None now; reserved for later |
| Power-loss safety | Strong (no FAT writes) | Strong only if `config.txt` is not rewritten | Strong |
| Risk | Low | High | Low now |

---

## 15. Decision record

- **Decision:** proceed with Option C. Build Option A in Phase 1 after the
  Phase 0 hardening, and reserve partitions/interfaces for a possible Option B.
- **Pending:** the owner's answer to Open Question 1 (app-only vs OS updates)
  determines whether Phase 2 is ever scheduled.

---

*End of update strategy.*
