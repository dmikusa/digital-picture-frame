# Photo Frame — Update & Device Threat Model

> **Provenance note.** The source threat-model report supplied with this task
> contained only a placeholder (`[THREAT MODEL REPORT] / PASTE_HERE`). This
> document is a reconstruction built from the current architecture
> (`plans/SPEC.md`, `plans/SYNC_SOURCES.md`, `docs/deployment.md`,
> `packaging/`, `.github/workflows/`) and the owner's stated constraints. It is
> written to be accurate and useful, but it is **not** a transcription of the
> original report. If the original report becomes available, replace the body
> below and keep this note until the two agree.

## How to use this document

This is the security baseline that `plans/UPDATE_STRATEGY.md` is derived from.
Use it in three ways:

1. **As a requirements source.** Section 8 distills the findings into
   MUST / SHOULD / NICE requirements. The strategy document maps each update
   option against that list.
2. **As a review checklist.** Before shipping an update mechanism or a new
   network-facing feature, check the relevant findings (Section 6) and confirm
   the mitigation or record an accepted risk.
3. **As a scoping tool.** Each finding has an ID (`T-nn`), a severity, and a
   gap. Turn accepted gaps into issues; do not silently drop them.

Severity is qualitative and considers this specific device (a single household
photo frame on a home LAN, often offline, with no hardware root of trust):

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

Record changes in the revision history at the end. Do not delete findings that
were mitigated; mark them mitigated with the commit or issue that closed them.

---

## 1. Scope and system description

A Raspberry Pi Zero 2 W running DietPi (Debian bookworm based, arm64) driving a
DRM/GBM/EGL photo frame. Two systemd services run as a dedicated `photo-frame`
user:

- `photo-frame-display` — C app, owns `/run/photo-frame/` and the image/control
  Unix sockets, renders photos and config/status screens.
- `photo-frame-manager` — Rust app, streams photo paths over the image socket,
  watches `/media` for USB imports, syncs Dropbox and Google Drive folders,
  and runs the config-mode admin HTTP server.

State lives on three places:

- **Photos + index + sync state**: a dedicated ext4 partition mounted at
  `/var/lib/photo-frame/photos` (`noatime,lazytime`), plus
  `sync-cursors.json` and `sync-status.json`.
- **Config + secrets**: `/etc/photo-frame/config.toml` (mode 0600, owned by
  `photo-frame`) and `/etc/photo-frame/display.env` (0644, root).
- **Ephemeral**: `/tmp/photo-frame.log`, `/tmp/photo-frame-display.log`, and
  `/tmp/photo-frame-setup.txt` on tmpfs.

The device is usually **not network-facing**. The admin server listens on
`0.0.0.0` only while config mode is active (USB-triggered or first boot), on a
random port in 8100–8199, behind per-session HTTP Basic Auth with an 8-character
password, and shuts down on finish, USB removal, or a 10-minute idle timeout.
Outbound connections happen only when remote sources are configured.

In scope: the running device, its data, its credentials, and the proposed
update mechanism. Out of scope: the owner's phone/laptop, the cloud providers'
own security, and physical destruction of the device.

## 2. Trust boundaries and assumptions

| Boundary | Trusted side | Untrusted side |
|----------|--------------|----------------|
| Home LAN | The device | Any LAN host, including guests/IoT |
| USB media | The device | Any inserted drive and its filesystem contents |
| Cloud folders | The device | Files placed in the synced folder by anyone |
| SD card / boot media | — | Anyone with physical access |
| Update artifacts | Device's pinned public key | Everything else (bundles, mirrors, CI) |
| CI / dependencies | — | Upstream crates, actions, ImageMagick |

Assumptions, stated explicitly:

- **A1.** There is no hardware root of trust: the Pi has no secure boot, no
  TPM/secure element, and no EEPROM bootloader (`tryboot` is a Pi 4/5 feature).
  Anyone with physical SD access can replace the boot chain.
- **A2.** There is no real-time clock on the Zero 2 W. Time comes from the
  network or a fake-hwclock file and can be wrong or attacker-influenced.
- **A3.** The owner is trusted and is the only signer of updates. The owner's
  devices and network are not assumed to be free of malware.
- **A4.** Cloud providers are assumed honest but their content is not trusted.
- **A5.** The device is single-tenant and low-value to a targeted attacker, but
  a good target for opportunistic scanning on a shared or guest network.

## 3. Assets

| Asset | Why it matters | Where |
|-------|----------------|-------|
| Photos | Personal/private, irreplaceable | Photos partition |
| Photo index, cursors, status | Rebuild cost; opaque cursors lose sync position | Photos partition |
| Dropbox tokens / app secret | Read access to the owner's Dropbox | `config.toml` |
| Google service-account private key | Long-lived, high-value; can read shared folders | `config.toml` |
| Google OAuth refresh token + client secret | Read access to the owner's Drive | `config.toml` |
| Admin session password | LAN access to the config UI | tmpfs + panel |
| Device identity | Hostname, Wi-Fi PSK, SSH host/user keys | rootfs `/etc` |
| System integrity | Binaries, units, boot chain, update log | rootfs, boot FAT, state |
| Update signing key | Forges trusted updates if leaked | Offline, off-device |

## 4. Threat actors

- **Opportunistic LAN attacker** — scans, probes the admin port, tries default
  or guessed credentials, sniffs cleartext HTTP.
- **Malicious/curious insider or guest** — on the same Wi-Fi; may share the
  synced cloud folder.
- **Cloud-content attacker** — can write to a synced folder; supplies crafted
  image files.
- **Malicious USB author** — leaves a crafted drive (images, filesystem, or a
  planted "update" bundle).
- **Physical attacker** — has the SD card or the device; can modify, downgrade,
  or clone it.
- **Supply-chain attacker** — compromises a crate, GitHub Action, or packaged
  dependency; or a maintainer token.
- **Owner as the signer** — not an adversary, but a key-loss risk.

## 5. Attack surface inventory

| Surface | Exposure | Notes |
|---------|----------|-------|
| Admin HTTP server (`0.0.0.0:8100–8199`) | LAN, config mode only | Basic Auth, plain HTTP, rotating password |
| Image/control Unix sockets | Local, same user | `IMG`, `CLR`/`SHOW`/`RESUME` |
| ImageMagick (`magick`/`convert`) | Spawned per import | Parses attacker-supplied images |
| Outbound HTTPS (Dropbox, Drive, token_uri) | Internet | TLS via native-tls |
| USB mounts under `/media` | Any inserted drive | Photo scan + conversion |
| `config.toml` writer | Local | Atomic temp+rename, service-user owned |
| Update path (proposed) | USB / LAN / optional online | None today; designed here |
| CI → GitHub Releases | Internet | Unsigned `.deb` artifacts today |

## 6. Threat findings

The table is the summary; notes follow for the findings that drive the update
strategy. "Gap" means a known missing control, not necessarily a defect to fix
now.

| ID | Threat | Vector | Impact | Existing mitigation | Gap | Severity |
|----|--------|--------|--------|---------------------|-----|----------|
| T-01 | Admin API credential guessing / timing | LAN brute force; non-constant-time compare | Full config-UI access; read/write cloud credentials | Per-session 8-char password, idle timeout, all routes authed | `==` compare, no rate limit, no lockout | Medium |
| T-02 | Cleartext admin transport | LAN sniffing/MITM | Password + cloud secrets exposed; staged config altered | Config mode only, short-lived | No TLS | Medium |
| T-03 | Credentials at rest | Physical SD access; root/service-user read | Long-lived Dropbox/Google access | 0600 file, 0750 dir, API redaction | No encryption, no keystore | High |
| T-04 | ImageMagick code execution | Crafted image via USB or cloud | RCE as `photo-frame` → credentials + persistence | Extension filter, `nice(10)` | No policy.xml hardening, no sandbox | High |
| T-05 | `token_uri` SSRF / secret redirect | Malicious `service_account_json` | Device POSTs a signed assertion to an arbitrary host; LAN probe | Scheme check only | No host allowlist; `http://` allowed | Medium |
| T-06 | Supply chain | Crate/action/dependency compromise; unsigned releases | Forged or backdoored binary delivered as trusted | Pinned action SHAs, `--locked`, pinned CVE-fixed fork, zizmor | Unsigned artifacts, no provenance/attestation | High |
| T-07 | Unsigned update installed | Malicious bundle over USB/network | Arbitrary root code execution | None (no updater yet) | Must be signature-gated | Critical |
| T-08 | Downgrade / replay | Reinstall an older validly signed bundle | Reintroduces fixed vulnerabilities | None | No monotonic sequence floor | High |
| T-09 | Physical boot-chain substitution | SD access; replace binaries/boot files | Full control, undetectable | None | No secure/verified boot, no dm-verity | High (accepted, A1) |
| T-10 | Malicious USB auto-update | Planted "update" drive | Root code execution | None | Auto-detection must require signature + owner action | High |
| T-11 | Archive/filesystem traversal | Crafted tar/filesystem in import or update | Overwrite files, plant units | Canonicalize on import | Future extractor must reject `..`, symlinks, abs paths, devices | Medium |
| T-12 | Secret leakage in logs/artifacts | Log lines, setup file, support bundles | Credential exposure | 0600 files, redaction, "never log key" | No automated log secret scanning | Low |
| T-13 | Resource exhaustion / photo loss | Admin flood; huge import; ENOSPC rotation | DoS; owner's photos deleted | Idle timeout, serialized imports, rotation | No rate limit; rotation deletes data | Medium |
| T-14 | Malicious cloud content | Write access to synced folder | Feeds T-04 | Read-only scopes | Image sandboxing | Medium |
| T-15 | Service-user config tampering | Post-exploit rewrite of `config.toml` | Persistence, new exfil sources | Atomic write | Config dir owned by the service user | Medium |
| T-16 | Time/clock manipulation | No RTC; spoofed NTP/hwclock | Misleading audit timestamps; timestamp-based rollback checks bypassed | — | Anti-rollback must be sequence-based, not time-based | Low |
| T-17 | Signing-key compromise | Leaked offline key or CI build key | Forged trusted updates | — | No rotation/revocation plan | Critical |
| T-18 | Failed update bricks device | Power loss, bad build, no rollback | Device down; reflash loses state | None | No two-phase/rollback | High |
| T-19 | Update-check privacy | Online check | Reveals device existence/IP/timing | — | Must be opt-in and minimal | Low |

### Findings that shape the update design

**T-07 / T-10 — the update channel is the crown jewel.** A naive updater is the
single highest-impact feature that could be added, because it turns a
low-interaction device into a root code-execution target. Every bundle must be
verified against a **pinned owner key** before any privileged action, and USB
auto-detection must never install without both a valid signature and an explicit
owner action.

**T-08 / T-16 — anti-rollback must be sequence-based.** Because there is no RTC
and no hardware root of trust, a monotonic counter stored on the device can only
defeat network/MITM downgrades and accidental downgrades. A physical attacker
can edit the counter on the same SD card. This limitation must be documented,
not hidden.

**T-09 / A1 — physical access is a full compromise.** Without secure boot, an
attacker with the SD card can replace the boot chain, the kernel, the binaries,
and the anti-rollback counter. No update design on this board can prevent that;
the goal is tamper-**evidence** (signed artifacts, dm-verity where used) and
recoverability, not tamper-**proofness**.

**T-18 — two-phase with automatic revert is mandatory.** App-only updates must
have a userspace boot guard that reverts if the new version fails its health
check, because there is no hardware A/B fallback. OS image updates need a
bootloader or recovery kernel to get the same guarantee.

**T-03 / T-04 / T-05 / T-15 — the credential blast radius.** A code-execution
bug in ImageMagick, or a redirect via `token_uri`, runs as the same user that
owns `config.toml`, so it reaches all cloud credentials. Hardening these is a
prerequisite for trusting any update mechanism (see Phase 0 in the strategy).

**T-02 / T-01 — the admin server is the only network listener.** It must stay
config-mode-only, and update checks must not create a second always-on listener.

## 7. Update-channel threats (specific)

These extend the general findings for the mechanism the strategy proposes.

| Threat | Description | Required control |
|--------|-------------|------------------|
| Forged bundle | Attacker crafts a bundle | Detached signature over the bundle by the pinned owner key; fail closed |
| Tampered artifact | Valid manifest, swapped binary | Per-artifact SHA-256 in the signed manifest, verified before install |
| Manifest swap | Attacker replaces manifest + signature | Signature covers the manifest and the payload hash; manifest names `key_id` |
| Replay / downgrade | Older signed bundle reinstalled | Monotonic `sequence` floor stored root-owned; reject `<= installed` |
| Wrong device/arch | arm64 bundle on another board, or wrong product | `product` + `arch` fields checked before install |
| Min-version bypass | Bundle assumes a newer config/ABI than installed | `min_version` (and `config_schema`) checked; refuse with a clear error |
| Partial write / power loss | Install interrupted | Verify whole file before use; atomic symlink flip; pending/confirmed markers; boot guard |
| Path traversal in bundle | Tar entries escape the install dir | Reject absolute paths, `..`, symlinks, hardlinks, devices; extract into a fresh dir |
| Bundle bombs / disk fill | Huge bundle fills the card | Check free space for bundle + unpacked size; cap member count/size |
| Auto-install from USB | Dropped drive installs silently | Require signature **and** an explicit owner marker/action |
| Online check privacy | Endpoint learns about the device | Opt-in, static minimal GET, no device ID, documented |
| CI key theft | Build key used to forge releases | Root/authority key stays offline; CI key cannot sign the root manifest |

## 8. Security requirements

Distilled from the findings above. The strategy document maps options A/B/C
against this list. "MUST" is a release gate for the update feature.

### MUST

| ID | Requirement | Source |
|----|-------------|--------|
| M1 | Authenticity: only bundles signed by the owner's pinned key install; verification fails closed. | T-06, T-07, T-17 |
| M2 | Integrity: every installed artifact's hash is verified against the signed manifest before use. | T-07 |
| M3 | Offline capability: a full update is installable from USB with no network. | Constraint |
| M4 | State preservation: photos, `config.toml`, `display.env`, index/cursors/status, hostname/Wi-Fi/SSH identity survive an update. | T-13, constraint |
| M5 | Two-phase + rollback: a failed update reverts automatically without reflashing; power loss is safe. | T-18 |
| M6 | No permanent exposure: update checks never create an always-on listener; online checks are opt-in. | T-02, T-19 |
| M7 | Audit logging: a persistent, root-owned record of every update attempt and result, separate from tmpfs logs. | T-16 |
| M8 | Anti-rollback floor: a monotonic sequence prevents accidental/network downgrade, with its limits documented. | T-08, T-16 |
| M9 | Verify before privilege: signature, arch, product, sequence, and hashes are checked before any root action. | T-07 |
| M10 | Extract safely: no path traversal, symlinks, hardlinks, devices, or writes outside the install dir. | T-11 |

### SHOULD

| ID | Requirement | Source |
|----|-------------|--------|
| S1 | Owner approval per install; no silent auto-update. | Constraint |
| S2 | Atomicity: installs are safe against power loss at every step. | T-18 |
| S3 | Key rotation and revocation without reflashing. | T-17 |
| S4 | Config schema versioning with forward migration. | T-16, M4 |
| S5 | Least privilege for the updater and its staging area. | T-15 |
| S6 | CI never holds the root/authority key; builds are attestable. | T-06 |
| S7 | Health check covers both services, sockets, and index availability. | M5 |
| S8 | Update failures are visible on the panel and in the audit log. | M7 |

### NICE

| ID | Requirement | Source |
|----|-------------|--------|
| N1 | Optional online check with a minimal, privacy-preserving request. | T-19 |
| N2 | Panel notification when an automatic revert happens. | M5 |
| N3 | Tamper-evident read-only root (dm-verity) for OS images. | T-09 |
| N4 | Release channels (stable/beta). | — |
| N5 | Power-loss safety aided by a small UPS/capacitor. | S2 |

## 9. Residual risk and accepted limitations

- **Physical access is a full compromise (A1, T-09).** No control on this board
  prevents a determined attacker with the SD card from replacing the boot chain
  or rolling back the anti-rollback counter. Accept and document; mitigate with
  tamper-evidence where practical.
- **No hardware keystore (T-03).** Cloud credentials remain readable by anyone
  who can read the card or reach the service user. Accept for a household
  device; reduce blast radius via Phase 0 hardening.
- **The service user owns its config (T-15).** Config mode must write
  `config.toml`, so the service user owns it. A post-exploit attacker can
  persist. Reduce by sandboxing and by limiting what the service user can reach.
- **Unsigned OS/dependency packages today (T-06).** The `.deb` is not signed and
  is fetched from GitHub Releases. Add signed releases in Phase 0.
- **Time is untrusted (A2, T-16).** Never base a security decision on the
  device clock.

## 10. Revision history

| Date | Change | Author |
|------|--------|--------|
| 2026-10-21 | Initial reconstruction (source report was a placeholder). | opencode |

---

*End of threat model.*
