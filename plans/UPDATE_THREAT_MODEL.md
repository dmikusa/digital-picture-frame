# Photo Frame — Update & Device Threat Model

## How to use this document

This is the security baseline that `plans/UPDATE_STRATEGY.md` is derived from.
Use it in three ways:

1. **As a requirements source.** Section 6 defines the prioritized
   MUST / SHOULD / NICE requirements (`M1`–`M9`, `S1`–`S7`, `N1`–`N5`). The
   strategy document maps each update option against that list.
2. **As a review checklist.** Before shipping an update mechanism or a new
   network-facing feature, check the relevant finding (`T-*`) and confirm the
   mitigation or record an accepted risk.
3. **As a scoping tool.** Each finding has an ID, an impact, and a gap. Turn
   accepted gaps into issues; do not silently drop them.

Severity in this document is qualitative and relative to a **consumer photo
frame on a home LAN**:

| Severity | Meaning |
|----------|---------|
| Critical | Compromise forges or installs code, or is trivially reachable. |
| High | Compromise yields credentials, code execution, or data loss with a plausible vector. |
| Medium | Realistic on the home LAN or with moderate effort, or limited impact. |
| Low | Requires unusual access, or impact is minor. |

## Living document

This document is **living**. The device, its dependencies, and the threats
around it change. Update it whenever any of the following happen:

- A new network-facing feature, socket, or outbound destination is added.
- The update mechanism changes (new transport, new signing scheme, new key).
- A dependency with a history of image/parser CVEs is added or removed.
- The OS, kernel, bootloader, or partition layout changes.
- A finding is mitigated, accepted, or invalidated.

Do not delete findings that were mitigated; mark them mitigated with the commit
or issue that closed them. Keep the revision history at the end current.

---

**Scope.** A Raspberry Pi Zero 2 W running DietPi, two systemd services
(`photo-frame-display`, `photo-frame-manager`) as the `photo-frame` user, a
dedicated ext4 photos partition, a LAN-only plain-HTTP admin server, USB/cloud
photo import, and **no update mechanism** today. This document is input for an
update-planning agent; it does not choose a final update design.

**Method.** Assets → trust boundaries/flows → actors → STRIDE threats →
existing mitigations/gaps → prioritized requirements → design implications.
Findings are grounded in the files listed in the brief. Severity is relative to
a consumer photo frame on a home LAN.

---

## 1. Assets

| # | Asset | Why it matters | Where it lives |
|---|-------|----------------|----------------|
| A1 | **Photos** (the family's library) | Irreplaceable, private | `/var/lib/photo-frame/photos` (ext4 partition), index CSVs |
| A2 | **Cloud credentials** — Dropbox static/refresh tokens, app secret; Google API keys, OAuth refresh tokens/client secrets | Account takeover, data exfiltration | `/etc/photo-frame/config.toml` (0600, `photo-frame`-owned) |
| A3 | **Google service-account private key** (RS256) | Impersonation of the service account; can mint `drive.readonly` tokens | `config.toml` `service_account_json` (plaintext) |
| A4 | **Update signing private key(s)** (future) | Root of trust for the whole device; compromise = arbitrary code as root | *Not present today*; must be offline |
| A5 | **Device availability / recoverability** | A frame that won't boot is a support burden and may need physical reflash | boot FAT partition, root ext4, services |
| A6 | **Update integrity** | A malicious update is arbitrary root code and persistent | update channel + local verification path |
| A7 | **User trust** | Owner must believe the device isn't exfiltrating photos or phoning home | whole system; update-check behavior |
| A8 | **Config-mode setup password** | Grants write access to `config.toml` (A2/A3) and network reachability of the admin API | screen QR, `/tmp/photo-frame-setup.txt` (0600) |
| A9 | **Photo index + cursors/status** | Losing them loses the library mapping or re-downloads everything | `photos_dir/*.csv`, `sync-cursors.json`, `sync-status.json` |
| A10 | **Display/control sockets** | Same-user control of the panel | `/run/photo-frame/*.sock` (0700 dir) |

---

## 2. Trust boundaries and data flows

```
[Internet]  --HTTPS(provider TLS)--> [manager: remote sync] --> photos dir
[LAN host]  --HTTP(plain, Basic)--> [admin server 0.0.0.0:8100-8199] --> config.toml
[USB drive] --auto-mount /media--> [usb watcher] --ImageMagick--> photos dir
[Owner/CI]  --GitHub Actions--> [unsigned .deb] --dpkg -i (root)--> device
[Owner]     --physical/SD--> [boot FAT + root ext4]
[manager]   --Unix sockets (0700)--> [C display app] --stb_image--> panel
[remote sync] --outbound only--> cloud providers
```

Boundaries, and what crosses them:

- **B1 USB ↔ device.** Untrusted filesystem image and untrusted file contents.
  Auto-mount is performed by `usbmount`/`dietpi-drive_manager`/`udisks2` (often
  as root); the app scans `/media` and shells out to ImageMagick. No signature,
  no allowlist, no sandbox.
- **B2 LAN ↔ admin server.** Plain HTTP, Basic Auth, binds `0.0.0.0`, 8-char
  password. Credentials and session content are exposed to any LAN sniffer.
- **B3 Device ↔ internet (cloud).** Outbound `reqwest`/native-tls only.
  Certificate verification is on (no pinning). Downloaded bytes are **not**
  content-verified.
- **B4 Build/CI ↔ device.** `.deb` artifacts are produced by GitHub Actions and
  installed with `sudo dpkg -i`; no signature, no checksum published. Trust is
  entirely "whatever is on the GitHub release."
- **B5 Physical/SD.** Root ext4 is unencrypted; boot FAT is unauthenticated and
  editable. Physical access is effectively root.
- **B6 Admin/control Unix sockets.** 0700 dir owned by `photo-frame`; both
  services run as the same user, so any code running as `photo-frame` (e.g., an
  ImageMagick RCE) can drive them.
- **B7 Update boundary (future).** Not yet defined. Whatever channel is chosen
  (USB, HTTPS, local file) crosses B1/B3/B4/B5 and needs its own authenticity,
  freshness, and rollback controls.

---

## 3. Threat actors

| Actor | Capability | Motivation |
|-------|-----------|------------|
| **LAN network attacker** | Sniff/inject plain HTTP; reach admin server; CSRF victim browsers; ARP/DNS spoofing | Steal config credentials, add a remote source, pivot to cloud accounts |
| **Malicious/compromised USB** | Crafted filesystem, crafted JPEG/HEIC, BadUSB-style device classes (HID/NIC) | RCE as `photo-frame`, credential theft, persistent tampering |
| **Cloud-account / provider compromise** | Upload malicious images to the synced folder; tamper with listings | RCE via image decoder, feed unwanted content, exfiltrate via `token_uri` |
| **Supply-chain / CI compromise** | Modify a workflow, dependency, action, or the published `.deb` | Ship a backdoored root package to every device |
| **Malicious update server / MITM** | Serve a valid-but-old or forged update, replay, or downgrade | Persistent root compromise or DoS |
| **Signing-key thief** | Forge legitimate-looking updates | Full trust compromise; hardest to detect |
| **Stolen device / SD-card attacker** | Read/write root and boot partitions | Read photos/secrets; inject `init=/bin/sh`; brick |
| **Local user (SSH/other process as `photo-frame`)** | Read secrets, write `config.toml`, hit sockets, race `/tmp` files | Escalate to config/cloud access |
| **Careless owner** | Power-cut during update, install from the wrong place | Self-inflicted bricking/rollback |

---

## 4. Threats (STRIDE-structured)

### 4.1 Spoofing

- **T-S1 (update authenticity bypass).** Today there is no update channel, so
  "spoofing an update" means "installing a forged `.deb`." An attacker who can
  place a file where the owner will `dpkg -i` it, or who can compromise the
  GitHub release, becomes root. *No signing exists.*
- **T-S2 (malicious/compromised update server).** If a future updater fetches
  over HTTPS and trusts the transport alone, a server compromise (or a
  CA/`native-tls` trust-store compromise) yields arbitrary root payloads.
  Certificate pinning is absent.
- **T-S3 (admin-server impersonation).** Plain HTTP + Basic Auth over the LAN:
  an attacker can capture the 8-char password from a single request (base64, not
  encryption). The QR also embeds the password in cleartext
  (`http://photo-frame:<pw>@<ip>:<port>`), so a photo of the screen is a
  credential.
- **T-S4 (cloud API impersonation).** A stolen service-account key (A3) lets an
  attacker mint tokens; a stolen Dropbox refresh triple is long-lived account
  access.

### 4.2 Tampering

- **T-T1 (update payload tampering / MITM).** No signature or checksum on
  artifacts; any MITM or malicious mirror can swap the payload. Requirement:
  detached signature over the artifact, verified on-device before install.
- **T-T2 (boot tampering).** The boot FAT partition is unauthenticated. An SD
  attacker can replace `kernel8.img`, edit `config.txt` (`init=/bin/sh`,
  `arm_64bit`, `os_prefix`), or add a malicious initramfs. There is **no Pi
  Zero 2 W secure boot** (confirmed: Raspberry Pi's own `rpi-sb-provisioner`
  states "No secure-boot support" for Zero 2 W), so firmware/kernel integrity
  cannot be hardware-anchored on this board.
- **T-T3 (image-decoder RCE).** Untrusted USB/cloud images are passed to
  `ImageMagick` (`convert_image`, `src/import/usb.rs:573-619`) as the
  `photo-frame` user, with no sandbox, no seccomp/AppArmor, no rlimits beyond
  `nice(10)`. ImageMagick has a long history of memory-corruption and
  delegate-based RCE (e.g., ImageTragick). The C app's `stb_image` JPEG decode
  is a second parser, though it normally only sees ImageMagick-normalized JPEGs.
- **T-T4 (filesystem-parser attack).** Auto-mounting an attacker filesystem
  exercises kernel ext4/vfat/exfat parsers (and USB gadget descriptors). Kernel
  bugs → root. Mitigation is kernel hygiene, not app code.
- **T-T5 (config tampering → SSRF).** `config.toml` is writable by
  `photo-frame`, and the sync scheduler hot-reloads it on mtime/size change
  (`ConfigReloader`, `src/import/remote.rs:204-256`).
  `service_account_json.token_uri` is attacker-controlled and used as the
  token-exchange URL (`docs/config.md:111`), so write access to config yields
  **SSRF to arbitrary internal/loopback URLs** plus JWT exfiltration. Anyone who
  obtains the admin password can do this through the API.
- **T-T6 (state tampering via `/tmp`).** The PID lock `/tmp/photo-frame.lock` is
  opened with create/truncate and no `O_NOFOLLOW`/ownership check
  (`src/main.rs:54-72`); `/tmp/photo-frame-setup.txt` and the QR/status JPEGs
  live in world-writable `/tmp`. A local attacker can pre-create symlinks or
  files to interfere (truncation, DoS) or read stale data.

### 4.3 Repudiation

- **T-R1 (no update audit trail).** With no update mechanism there is also no
  record of what was installed, when, by whom, or from where. A future design
  must log update attempts and results; note logs currently live on **tmpfs**
  (`/tmp/photo-frame.log`), so they vanish on reboot and are unsuitable as the
  sole audit record.
- **T-R2 (sync actions unlogged long-term).** `sync-status.json` is overwritten
  per attempt and lives on the photos partition; there is no append-only
  history.

### 4.4 Information disclosure

- **T-I1 (config secrets).** `config.toml` (A2/A3) is 0600 under a 0750 dir,
  which is reasonable, but it is readable by any process as `photo-frame` —
  including a compromised manager or an ImageMagick RCE. It is also plaintext
  at rest on an unencrypted SD card.
- **T-I2 (admin credential sniffing).** B2: any LAN host can capture Basic Auth
  over HTTP. No HSTS/TLS exists.
- **T-I3 (QR/screen disclosure).** The password is rendered in the QR URL and
  printed on screen (`src/qr.rs`), and mirrored to
  `/tmp/photo-frame-setup.txt`. Shoulder-surfing or a camera gets a working
  credential for the session.
- **T-I4 (photo privacy).** Photos are unencrypted at rest and readable by
  removing the SD card.
- **T-I5 (update-check telemetry).** Any online update check leaks device IP,
  current version, update cadence, and presence/absence of the device to the
  update server. The owner explicitly does not necessarily want auto-updates; an
  always-on check is a privacy regression.
- **T-I6 (error strings).** Sync errors are surfaced through `/api/status` and
  logs; provider error bodies could echo sensitive detail. Redaction exists for
  known param names only (`SENSITIVE_PARAMS`, `src/admin_server.rs:476-485`); an
  unknown-named secret would not be redacted.

### 4.5 Denial of service / bricking

- **T-D1 (power loss mid-update).** The Zero 2 W has no battery/UPS; a power cut
  during a kernel/boot-partition write can leave an unbootable device with no
  recovery path. FAT boot files are not atomically replaced (rename on vfat is
  not crash-safe in the same way as ext4).
- **T-D2 (bad update, no rollback).** If a future update replaces the root
  filesystem or kernel in place, a bad build crash-loops and there is no second
  slot. `Restart=on-failure` on both units will happily loop forever on a bad
  binary.
- **T-D3 (no A/B or watchdog on this board).** The Pi 4/CM4
  `tryboot`/`autoboot.txt` one-shot A/B mechanism is an EEPROM-bootloader
  feature. Zero 2 W boots via GPU ROM → `bootcode.bin` → `start.elf` →
  `kernel8.img` from the FAT partition with no EEPROM, so there is **no native
  boot-retry/fallback**. Any A/B design must add a bootloader (U-Boot/initramfs)
  or do a userspace file swap with a separate watchdog — both are weaker than a
  hardware-anchored scheme.
- **T-D4 (storage exhaustion).** Photos fill the partition; an update that
  stages a full image on the same SD card could run out of space mid-write
  (`ENOSPC` handling exists only for photos).
- **T-D5 (admin-server brute force/DoS).** No rate limiting or lockout on Basic
  Auth; `check_auth` uses a non-constant-time string compare
  (`src/admin_server.rs:240-252`). The 10-minute idle timeout and per-session
  rotation limit the window, but a LAN attacker can hammer it and also reset the
  idle timer on every request (`last_request` is updated in `handle_request`),
  keeping the server alive.
- **T-D6 (CSRF on admin API).** No CSRF token, no Origin/Referer check. A
  malicious page on the same LAN can issue cross-origin form posts
  (`application/x-www-form-urlencoded` is a CORS "simple request") to
  `/api/finish` or `/api/sources/dropbox`; the browser will attach cached Basic
  Auth. The response is unreadable to the attacker, but the side effect (adding
  a source, saving config) happens.
- **T-D7 (update-channel DoS).** A malicious update server can refuse to serve,
  serve an oversized/slow payload, or claim no updates — availability risk for
  opt-in updates. `http.rs` bounds retries/backoff (30 s cap) and timeout
  (120 s), which helps.

### 4.6 Elevation of privilege

- **T-E1 (unsigned update → root).** `dpkg -i` runs `postinst` as root
  (`packaging/deb-scripts/postinst`); a malicious `.deb` owns the device. This is
  the single highest-impact update threat.
- **T-E2 (signing-key compromise).** An attacker with A4 can sign arbitrary
  updates that verify correctly. Key custody, rotation, and revocation dominate
  the update design.
- **T-E3 (ImageMagick → `photo-frame`).** RCE in the converter yields secrets
  (A2/A3), config write (T-T5), socket control, and persistence via
  `config.toml`/cron-like vectors. Not root by itself, but combined with any
  local root bug it is a stepping stone; and `photo-frame` can already read all
  cloud credentials.
- **T-E4 (service hardening gap).** Neither unit sets `NoNewPrivileges`,
  `ProtectSystem`/`ProtectHome`, `PrivateTmp`, `PrivateDevices`,
  `ProtectKernel*`, `RestrictAddressFamilies`, `SystemCallFilter`,
  `CapabilityBoundingSet`, or `ReadWritePaths`. The manager can write all of
  `/etc/photo-frame` and the photos partition. There is no `sudo` exposure
  (shell is `/bin/false`), which is good.
- **T-E5 (display fallback to root).** `docs/deployment.md:140-159` documents
  running the display service as `root` if `video` group access fails. That turns
  a display-app memory-safety bug (parsing rendered JPEGs / DRM ioctls) into
  root.
- **T-E6 (rollback abuse).** Because a valid signature stays valid forever, a
  remote attacker who can influence update selection could pin the device to an
  old, vulnerable-but-signed release. Anti-rollback needs a persistent,
  monotonic version floor (and note a physical attacker can reset any floor
  stored on the SD card).

---

## 5. Existing mitigations and gaps

**What is already done well**

- Both services run as a dedicated non-root `photo-frame` user with
  `/bin/false`; no sudo.
- `config.toml` is 0600 in a 0750 `photo-frame`-owned directory; atomic writes
  use a 0600 temp file + `rename` (`src/config.rs:166-213`).
- Setup credentials file is 0600 and removed on every config-mode exit and on
  shutdown (`src/config_mode.rs`, `src/main.rs:346-350`).
- Config-mode password is per-session, random 8-char alphanumeric, and the
  server is stopped on USB removal/timeout.
- Admin API redacts known credential parameter names; error messages avoid
  echoing key material (tests assert this).
- Remote sync is outbound-only with bounded retries/backoff and a total request
  timeout; TLS cert verification is on (native-tls), and `cargo build --locked`
  plus SHA-pinned GitHub Actions reduce CI drift.
- `/run/photo-frame` is 0700 and owned by the display unit; `umask(077)` in the
  C app as defense in depth.
- Downloads stage to a hidden `.sync-tmp` with `safe_filename()` traversal
  protection.
- `tiny_http_fork =0.12.8` is a CVE-fixed fork pinned deliberately.
- The QR encodes the password to make setup one-scan and offline-friendly (a
  usability/safety tradeoff).

**Gaps that matter for updates and current exposure**

- **No artifact signing or checksums anywhere** in `build.yml`; releases are
  unsigned and `GITHUB_TOKEN` with `contents: write` can publish them.
- **No update mechanism at all** → no rollback, no A/B, no boot integrity, no
  version floor, no audit.
- **No secure boot / no hardware root of trust on Zero 2 W**; boot FAT is
  unauthenticated.
- **No OS-level sandboxing** for ImageMagick or the services; the converter
  processes untrusted input as the credential-owning user.
- **Plain HTTP admin API** with no TLS, no CSRF protection, no rate limiting,
  non-constant-time auth compare.
- **No content verification** of cloud-downloaded images; provider/MITM
  compromise feeds the same ImageMagick surface.
- **`token_uri` SSRF** via writable `config.toml`.
- **Logs on tmpfs** and overwritten status files → weak auditability.
- **`/tmp` files** with no symlink/ownership hardening.
- **No anti-rollback primitive** and no place an attacker can't reset it.

---

## 6. Security requirements for an update design (prioritized)

### Must

1. **M1 — Authenticated updates.** Every update artifact is verified on-device
   against a public key that is not obtainable or replaceable by the update
   channel itself. Signature check happens *before* any privileged action;
   unsigned/invalid → refuse, log, no partial install.
2. **M2 — Integrity covers the whole artifact set** (binaries, systemd units,
   any scripts executed as root), not just a manifest.
3. **M3 — No root execution without verification.** `postinst`-equivalent
   actions must run only after verification; no "install then verify."
4. **M4 — Crash/power-fail safety and a rollback path.** An interrupted or bad
   update must not brick the device; there must be a way to boot the previous
   working version or a recovery mode, and the device must not depend on a
   network or display to recover.
5. **M5 — Fail-closed.** If verification, signature-key handling, or version
   checks fail, the system keeps running the current version rather than
   proceeding.
6. **M6 — Anti-rollback (as far as the platform allows).** Reject versions below
   a persisted floor. Be explicit that a physical/SD attacker can defeat a
   software-only floor.
7. **M7 — Preserve state.** Photos partition, `config.toml`,
   index/cursors/status, Wi-Fi/SSH config, and host keys survive updates;
   `config.toml` is never clobbered by a package's default (`.deb` `conffiles`
   behavior must be preserved or superseded).
8. **M8 — Least privilege for the update path.** The updater should not run as
   root beyond the minimum, and must not be reachable/triggerable by an
   unauthenticated LAN actor.
9. **M9 — Offline-capable.** Verification and installation must work with
   USB/local media and no internet (device is often offline), so signing cannot
   rely on an online OCSP/CA check.

### Should

10. **S1 — Explicit opt-in, no silent auto-update.** Checks and installs are
    owner-initiated or owner-approved; no background install.
11. **S2 — Minimal, opt-out update checks with no device-identifying
    telemetry** (no stable unique ID; coarse version only; no per-device
    heartbeat). Default to no check if the owner hasn't opted in.
12. **S3 — Signed metadata + expiry/sequence.** A signed manifest with a
    monotonic sequence number and optional freshness date defends against replay
    and freeze attacks.
13. **S4 — Audit log on persistent storage** (not just tmpfs): version
    before/after, source, signature key id, result.
14. **S5 — Key rotation and revocation** with a documented offline process;
    support a new key without a reflash.
15. **S6 — Update-path sandboxing / atomicity**: verify and stage in a separate
    location, then swap; never write the running root in place.
16. **S7 — Recovery tooling** documented for the owner (how to inspect/verify an
    image offline, how to recover a failed device without losing photos).

### Nice

17. **N1 — Reproducible builds** and published hashes/SBOM for the `.deb`/image,
    so the owner can independently verify CI output.
18. **N2 — Optional dm-verity / read-only root** to detect root-filesystem
    tampering (given no secure boot, it's tamper-evident, not tamper-proof).
19. **N3 — Encrypted photos partition** (LUKS with a key stored away from the SD
    card) to protect A1 on theft — separate from updates but interacts with
    recovery.
20. **N4 — Rollback command** the owner can run intentionally, and a documented
    "hold" to pin a known-good version.
21. **N5 — Sign the CI artifacts at build time** with a key not exposed to the
    general-purpose build job, or at minimum publish checksums signed by A4
    out-of-band.

---

## 7. Design implications

**Key management**

- A4 must be **offline** (hardware token or air-gapped machine), used only to
  sign releases; the build/CI environment should not hold it. If CI must sign,
  use a protected environment/key with audit and no ability to exfiltrate, and
  treat that as a distinct high-value asset.
- The device needs the **verification public key** in a location the update
  channel cannot rewrite (e.g., baked into the rootfs and additionally pinned in
  a root-owned, immutable file; ideally in a partition the updater mounts
  read-only). On Zero 2 W there is no OTP/secure-boot anchor, so this is a
  software trust anchor — call that out honestly.
- Plan **key rotation/revocation** from day one: support N trusted keys with key
  IDs, and a signed "revoke key X" mechanism, plus an owner recovery path if the
  key is lost (likely: reflash).

**Signing format**

- Prefer a **detached signature over the exact bytes distributed** (e.g.,
  minisign/Ed25519 or GPG detached over the `.deb`/image), plus a **signed
  manifest** containing version, monotonic sequence, target arch, artifact hash,
  and a min-version floor. Keep it simple enough to verify offline with minimal
  trusted code (a small verifier, not the full manager).
- Include arch (`arm64`) and a device/product identifier so an amd64 `.deb`
  can't be installed by mistake (also a safety, not just security, control).

**Offline verification**

- Verification must not require network. USB-based updates should be verified
  with the same code path as network updates. Consider requiring the owner to
  confirm the update on the device screen (but note the display may be the thing
  that's broken — provide a headless/SSH path).

**Rollback / boot integrity (the hard part on Zero 2 W)**

- There is **no `tryboot`/`autoboot.txt` and no secure boot** on this board; the
  firmware reads a fixed `kernel8.img`/`config.txt` from unauthenticated FAT.
  Options, all with caveats:
  - *Userspace swap + watchdog*: write a new root/kernel to a second location,
    flip a root-owned "boot slot" marker, reboot, and have a userspace watchdog
    mark the boot good within N seconds or revert. Weakness: the marker and FAT
    files are writable by root/SD, FAT writes aren't atomic, and a power cut
    during the flip is dangerous. Must make the flip a single atomic rename on
    ext4 where possible.
  - *Add a bootloader/initramfs* (U-Boot or a minimal initramfs on the FAT
    partition) to implement real A/B and optionally dm-verity. This adds boot
    complexity and its own attack surface, but it is the only way to get a
    genuine boot-time rollback and verify the rootfs.
  - *Recovery partition / initramfs fallback*: a small, rarely-changed recovery
    image that can restore a known-good slot; essential given T-D1.
- **Anti-rollback** needs a monotonic counter. A signed "min sequence" stored on
  the root ext4 works against remote attackers (they can't write it) but not
  physical ones; there is no OTP/eFuse counter readily usable here. State this
  limitation explicitly rather than implying rollback protection against SD
  access.

**Partition layout**

- Current layout is boot FAT + root ext4 + photos ext4 (created manually,
  `PARTUUID` in fstab). A/B needs at least a second root slot; the card is small
  and the root was deliberately kept ~900 MB. Consider: boot FAT, root A, root
  B, photos, and possibly a small persistent state partition. Photos and
  `/etc/photo-frame` must be outside the slots (or symlinked/mounted) so an
  update never touches them. Watch total card size and T-D4.

**Preserving state/config**

- `config.toml` is service-owned and rewritten by config mode; a package/image
  update must not overwrite it, and the `.deb` `conffiles` behavior (documented
  in `docs/deployment.md:186`) must be preserved or replaced with an equivalent
  merge policy. Preserve `sync-cursors.json`, `sync-status.json`, the index
  CSVs, `display.env`, and system identity (hostname, Wi-Fi, SSH host keys)
  across updates. Define what happens if the new version needs a config
  migration (schema/version field in `config.toml`).

**Opt-in vs auto-update**

- The owner does not necessarily want auto-updates and may be offline. Design
  for **owner-initiated updates** (USB or a manual command), with an optional,
  minimal, opt-in check. Do not run a background installer as root. If a check
  is offered, make it privacy-preserving (no stable device ID, no heartbeat, no
  version history beyond a single request).

**Logging / audit**

- Move the update audit trail off tmpfs to persistent storage (root partition or
  a dedicated state area), recording: timestamp, from-version, to-version,
  artifact hash, key ID, source, and result. Keep the existing tmpfs logs for
  operational noise. Ensure the audit record is not trivially rewritable by a
  compromised `photo-frame` process (root-owned, append-only where possible).

---

## 8. Assumptions and open questions for the planning agent

- **A-1:** Confirm the exact DietPi/Zero 2 W boot flow in use (no EEPROM, no
  `tryboot`) before committing to an A/B design; the report assumes the classic
  ROM → `bootcode.bin`/`start.elf` → `kernel8.img` flow and no secure boot.
- **A-2:** Decide whether updates target **only the app**
  (`photo-frame-manager`/`photo-frame-display` + units) or the **whole
  OS/kernel**. App-only updates are far easier to make safe and can reuse the
  existing `.deb` path; OS/kernel updates force the boot-integrity/rollback
  problem.
- **A-3:** Decide the **transport**: signed artifacts on USB (works offline,
  needs physical access) vs HTTPS pull (convenient, needs a server and privacy
  controls). They are not mutually exclusive; verification should be identical.
- **A-4:** Confirm whether the owner is willing to accept a **software-only
  trust anchor** (public key in rootfs) given there is no secure boot, and
  document the residual physical-attack risk.
- **A-5:** Decide the **failure policy**: if an update fails verification
  mid-stream, does the device stay on the current version silently or surface an
  error on the panel/`/api/status`?
- **A-6:** Any update design should also address the **pre-existing** exposure
  (plain-HTTP admin API, ImageMagick sandboxing, `token_uri` SSRF, systemd
  hardening), because an unsigned/malicious update is not the only path to
  root-adjacent compromise on this device.

---

**Bottom line.** The update problem here is dominated by one hard fact: the Pi
Zero 2 W has no hardware root of trust and no native A/B/rollback, so update
integrity must be **signed and verified in userspace**, and rollback/boot
integrity must be engineered (userspace swap + watchdog or an added
bootloader/initramfs) with an explicit acknowledgement that a physical SD
attacker can bypass all of it. Authenticity and fail-closed behavior are
must-haves; anti-rollback and offline verification are must/should; opt-in,
privacy-preserving checks and persistent audit are should/nice. The current
device already has meaningful exposure (unsigned artifacts, plain-HTTP admin
API, unsandboxed ImageMagick on untrusted input, writable-config SSRF) that an
update design should not ignore.

---

## 9. Revision history

| Date | Change | Author |
|------|--------|--------|
| 2026-10-21 | Initial report; consumed by `plans/UPDATE_STRATEGY.md`. | — |

---

*End of threat model.*
