# syauth

> **Phone-as-key Linux unlock.** Sign your `sudo`, `login`, `gdm`, and
> `swaylock` with a biometric tap on the Android phone in your pocket
> — no password typing, no shared secrets on disk, no relay-attack
> footgun. When the phone is out of range, syauth steps aside and
> FIDO2 or your password handles the auth.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2024-orange)](Cargo.toml)
[![Platform](https://img.shields.io/badge/platform-Linux%20%2B%20Android-lightgrey)](#)
[![Status](https://img.shields.io/badge/status-v0.1%20RC-yellow)](specs/syauth/ROADMAP.md)

---

## Why

Every existing "phone unlock" for Linux is one of:

- **Passive Bluetooth proximity** — a paired phone in range grants
  unlock. Trivially broken by an LE relay attacker.
- **`pam_u2f` only** — works, but the U2F key has to be on your
  desk; your phone is already in your pocket.
- **TOTP / Krypton** — needs a network round-trip, doesn't survive
  offline use, no biometric per-unlock guarantee.

syauth threads the needle with three guarantees that the prior art
doesn't carry together:

1. **Cryptographic challenge–response.** Per-unlock random nonce
   signed by the phone with Ed25519. Replay-proof, MITM-proof.
2. **Per-unlock biometric.** The signing key lives in the Android
   Keystore with `setUserAuthenticationRequired(true)`. A
   stolen-but-unlocked phone can't sign. A relay attacker can't sign
   either — the biometric prompt fires on the bonded phone, not
   theirs.
3. **Graceful fallback.** PAM stack control flag is `sufficient`,
   not `required`. Phone absent → next module runs. The default
   install wires FIDO2 as the fallback so the chain reads
   `syauth → FIDO → password`.

---

## How it works

```
┌─────────────────┐                 BLE/LESC                 ┌──────────────────┐
│  Linux desktop  │                                          │  Android phone   │
│                 │                                          │                  │
│  pam_syauth.so  │ ── challenge (nonce, MAC) ─────────────► │  approve screen  │
│       │         │                                          │        │         │
│       ▼         │                                          │        ▼         │
│ syauth-presenced│ ◄──── response (Ed25519 signature) ───── │  BiometricPrompt │
│   (user daemon) │                                          │ + Keystore sign  │
└─────────────────┘                                          └──────────────────┘
        │                                                              ▲
        │            verify(VerifyingKey, body, signature)             │
        └──────────────────────────────────────────────────────────────┘
                       (32-byte Ed25519 public key,
                        pinned at pair time via LESC
                        + 4-word app-level OOB confirm)
```

- The desktop **advertises** a rotating session UUID derived from
  `BLAKE3(bond_key || current_minute)`. The phone observes presence
  via `CompanionDeviceManager` and opens a GATT client with
  `autoConnect=true`.
- Pairing uses **BLE LE Secure Connections + numeric comparison**
  (the 6-digit code) AND an **app-level 4-word OOB confirm** on top,
  so the bond survives a future MITM in the LESC pairing itself.
- The phone's Ed25519 private key is **minted on the phone** at pair
  time and **never leaves the Keystore**. Each `sign()` call
  triggers a fresh `BiometricPrompt`.

---

## Quick start

### 1. Build & install the desktop side

```sh
git clone https://github.com/dmytrogajewski/syauth.git
cd syauth
cargo build --release \
  -p syauth-cli -p syauth-pam -p syauth-presenced

sudo install -m 644 target/release/libpam_syauth.so \
  /usr/lib64/security/pam_syauth.so
sudo install -m 755 target/release/syauth          /usr/local/bin/syauth
sudo install -m 755 target/release/syauth-presenced \
  /usr/local/libexec/syauth-presenced

syauth install-presenced --live
```

### 2. Install the Android app

```sh
cd syauth-android
./gradlew :app:assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

(Or grab a signed APK from the
[Releases](https://github.com/dmytrogajewski/syauth/releases) page.)

### 3. Pair

```sh
syauth pair --waybar      # desktop side; surfaces the 6-digit code in the bar
```

On the phone: tap **Pair**, pick the desktop from the OS picker,
confirm the 6-digit LESC code, then confirm the four-word OOB
phrase. Bond is persisted; from now on the desktop's daemon and the
phone's foreground service hold a long-lived link.

### 4. Wire it into your PAM stacks

```sh
sudo syauth install-pam --service sudo
sudo syauth install-pam --service gdm-password
sudo syauth install-pam --service swaylock
sudo syauth install-pam --service login --with-u2f-fallback
sudo syauth install-pam --service su    --with-u2f-fallback
```

Defaults: `--control sufficient` and `--module-args timeout=8000`.
The tool writes a `.bak` snapshot per service, so undoing is
`syauth uninstall-pam --service <name>`.

### 5. Verify

```sh
sudo true                 # phone vibrates → tap biometric → root shell
sudo journalctl --since "30 seconds ago" | grep grantors=pam_syauth
```

If you see `grantors=pam_syauth`, you're done.

---

## Resulting auth chains

| Service        | Chain                                                |
|----------------|------------------------------------------------------|
| `sudo`         | syauth (8 s) → `pam_u2f` cue → `system-auth`         |
| `gdm-password` | syauth (8 s) → selinux\_permit → `pam_u2f` cue → password-auth |
| `swaylock`     | syauth (8 s) → `pam_u2f` cue → include `login`       |
| `login`        | syauth (8 s) → `pam_u2f` cue → `system-auth`         |
| `su`           | syauth (8 s) → `pam_u2f` cue → `pam_rootok` → `system-auth` |
| `sshd`         | _intentionally not installed_ (no phone presence over SSH) |

Every line above is what `head -6 /etc/pam.d/<service>` actually
prints on a freshly-provisioned host.

---

## Resilience

A daemon restart used to leave the phone's CCCD subscription bound
to a dead GATT application registration, killing every subsequent
challenge. Two changes make recovery automatic:

- **Desktop** (`crates/syauth-transport/src/peripheral.rs`): after
  registering a fresh GATT app, the daemon iterates
  `adapter.device_addresses()` and calls `Device::disconnect()` on
  any connected peer. The phone's link drops cleanly.
- **Phone** (`syauth-android/.../bg/PersistentGattClient.kt`): on
  every `STATE_CONNECTED`, the client calls `BluetoothGatt.refresh()`
  (reflective; clears the on-disk service cache) before
  `discoverServices()`.

End-to-end recovery: ~8 seconds, no human intervention. The next
`sudo` succeeds via syauth, not FIDO fallback.

---

## What's where

```
syauth/
├── crates/
│   ├── syauth-core/           # Wire format, BLAKE3 MAC, OOB derivation, fuzz harness
│   ├── syauth-transport/      # bluer GATT peripheral + advertisement rotation
│   ├── syauth-pam/            # pam_syauth.so — auth-stack entry point
│   ├── syauth-cli/            # syauth(1): pair / list / revoke / status / install-pam / doctor
│   ├── syauth-presenced/      # Long-running user daemon (systemd --user unit)
│   └── syauth-mobile/         # UniFFI bindings consumed by the Android app
├── syauth-android/            # Kotlin app (Compose UI, Keystore signing, CDM presence)
├── specs/
│   ├── syauth/SPEC.md         # Protocol, wire format, install layout
│   ├── syauth/ROADMAP.md      # Items S-001..S-019 + JOURNEY closures
│   └── threat/                # Formal threat model (T-001..T-016)
├── docs/                      # Setup guides, security overview, known gaps
└── scripts/                   # e2e-unlock.sh latency benchmark, build helpers
```

---

## Configuration cheatsheet

```sh
# Daemon
systemctl --user status syauth-presenced
journalctl --user -u syauth-presenced -f
syauth status                     # adapter + bonded peers + last unlock
syauth status --json              # same data, machine-readable

# Bonds
syauth list                       # TSV: id, name, status, created_at
syauth revoke --id <peer_id>      # idempotent; audit trail preserved
syauth pair --force               # overwrite an existing bond record

# Health
syauth doctor                     # one OK/WARN/FAIL line per probe
syauth doctor --json              # typed JSON for tooling
```

Environment overrides for the daemon are in the systemd unit
(`~/.config/systemd/user/syauth-presenced.service`); see
`docs/known-gaps.md` for an audit-trail of every spec deviation.

---

## Security model

A formal threat model lives in
[`specs/threat/THREAT-2026-05-15.md`](specs/threat/THREAT-2026-05-15.md);
the short version:

- **T-001..T-006** (link-layer attacks): covered by LESC + per-unlock
  signing.
- **T-007** (compromised phone): bound by the Keystore's
  `setUserAuthenticationRequired(true)` — a stolen unlocked phone
  cannot sign without a fresh biometric.
- **T-014** (biometric coercion / phishing prompt): hostname is
  sanitized + truncated on the Approve screen so a malicious peer
  can't render a multi-line phishing prompt.
- **T-016** (compromised desktop): in scope for v0.2; v0.1 trusts
  the desktop's `bond_key`.

When in doubt, the failure mode is `pam_syauth` returning
`PAM_AUTHINFO_UNAVAIL` and the auth stack falling through to FIDO /
password — never an exception, never a fail-open.

---

## Roadmap

- **v0.1.0 (RC, current)** — five auth surfaces (sudo, login, su,
  gdm-password, swaylock), real-device LESC, Keystore-resident
  Ed25519, FIDO2 fallback installed in one CLI command.
- **v0.2** — F-Droid + Play Store delivery, multi-host bonds,
  bond-revocation push from desktop, daemon presence on the system
  bus.
- **v0.3** — pre-boot unlock (LUKS/cryptsetup), CompanionDeviceService
  in-process re-discovery so the daemon kick is no longer needed.

Tracking is in
[`specs/syauth/ROADMAP.md`](specs/syauth/ROADMAP.md). Sibling
roadmap for the `sy` desktop integration is at
[`~/sources/sy/specs/roadmaps/syauth-integration/ROADMAP.md`](https://github.com/dmytrogajewski/sy/blob/master/specs/roadmaps/syauth-integration/ROADMAP.md).

---

## Contributing

PRs welcome. The repo's contract is in
[`AGENTS.md`](AGENTS.md); short version:

- `cargo clippy --all-targets -- -D warnings` is the gate, not the
  guideline.
- Every new module ships with tests before behaviour.
- No `TODO` / `FIXME` / `unimplemented!()` in committed code — the
  pre-commit hook blocks it.
- Open SPEC deviations live in
  [`docs/known-gaps.md`](docs/known-gaps.md) with a numbered
  `DEV-NNN` audit row.

---

## License

[MIT](LICENSE). Copyright © 2026 syauth contributors.
