# SPEC: syauth — Phone-as-Key Unlock for Linux

> Status: draft research spec
> Author: `/researcher` skill
> Date: 2026-05-14
> Sibling project mirrored for architecture: `~/sources/prrr`

## 1. Summary

syauth is a **phone-presence-plus-consent** authenticator for Linux. A small Android companion app holds an ML-KEM/Ed25519 keypair bonded to a desktop. When PAM is invoked (login, sudo, lockscreen), `pam_syauth.so` sends a one-shot challenge to the paired phone over a low-latency channel; the phone — after a fresh user gesture (biometric or screen-on tap) — signs and returns the response. On success, PAM admits the user. On peer absence, peer denial, or any timing/integrity failure, PAM falls through to the next module in the stack. The system is **not** a passive proximity unlock: every unlock requires an explicit user action on the phone, by design, because passive BLE proximity has been comprehensively broken by link-layer relay attacks.

## 2. Background & Research

### 2.1 Market Context

| Product | Transport | Pairing | User gesture per unlock? | Key weakness |
|---------|-----------|---------|--------------------------|--------------|
| **Apple Auto Unlock** (Watch → Mac) | BLE + Wi-Fi P2P time-of-flight | Apple ID + Secure Enclave-to-Secure Enclave STS | No (passive, ≤ 2-3 m) | Requires custom silicon (Secure Enclave) and Wi-Fi RTT on both sides; closed protocol |
| **MagicLogon / DuoSecurity** (vendor) | Push over TLS/Internet | Account-bound | Yes (approve push) | Requires Internet, cloud trust root |
| **KDE Connect** (presence + actions, not PAM auth) | mDNS + TLS over LAN | Local cert pair on first connect | No (presence only) | LAN-bound, no distance bounding, no PAM module |
| **pam-bluetooth / pam_blue / pam-beacon** (FOSS PAM) | BR/EDR or BLE presence | Standard BT pairing | No (passive) | All vulnerable to NCC link-layer relay (≤ 8 ms) |
| **BLEUnlock** (macOS, FOSS) | BLE RSSI | iBeacon-style | No (passive RSSI threshold) | Same as above, plus RSSI is easily spoofed |
| **Windows Hello Companion Device Framework** (deprecated) | BLE | Microsoft account | No | Deprecated by Microsoft; reliability cited as cause |

**Takeaways:**
- Every shipping FOSS PAM-over-Bluetooth product is a passive proximity check. None survive the NCC link-layer relay attack ([NCC Group, 2022](https://research.nccgroup.com/2022/05/15/technical-advisory-tesla-ble-phone-as-a-key-passive-entry-vulnerable-to-relay-attacks/)) — relayed unlocks succeed with as little as 8 ms added latency, circumventing encrypted BLE entirely.
- The only deployed system that resists relay attacks at the protocol level is **Apple Auto Unlock**, which augments BLE with Wi-Fi peer-to-peer ranging at the speed of light. The cost is custom silicon and tight OS integration.
- The only attack-cheap, hardware-cheap defense is **explicit user interaction per unlock** (an approve-tap, a fingerprint, a face-scan on the phone). This is what NCC recommends to deployers who cannot ship UWB/RTT ([NCC mitigation guidance](https://research.nccgroup.com/2022/05/15/technical-advisory-tesla-ble-phone-as-a-key-passive-entry-vulnerable-to-relay-attacks/)).

### 2.2 Technical Context

**Transports evaluated:**

| Transport | Latency | Range | Background-survivable on Android | Distance bounding | Verdict |
|-----------|---------|-------|----------------------------------|--------------------|---------|
| **BLE GATT** (notify) | 50–300 ms typical | ~10 m | Yes, with `CompanionDeviceService` + `REQUEST_COMPANION_RUN_IN_BACKGROUND` | No (RSSI is spoofable, link-layer relay defeats it) | **Primary** (with mandatory user gesture) |
| Wi-Fi RTT (802.11mc/az) | Sub-ms | 5–30 m | Only when app foregrounded; Android RTT API is per-scan | **Yes**, time-of-flight at c | Optional secondary signal; device support spotty pre-2022 |
| Wi-Fi LAN (mDNS + TLS, KDE Connect-style) | 5–50 ms | Whole subnet | Yes (foreground service) | No (LAN scope is too wide for "presence") | Useful **fallback** when BLE adapter absent; not primary |
| UWB (IEEE 802.15.4z) | Sub-ms | 30 m | Yes on Android 13+ Pixel 6 Pro / 7+ / Samsung S21+ Ultra+ | **Yes**, sub-cm | Not yet ubiquitous; defer |
| NFC | <100 ms | ~4 cm | Foreground intent only | Implicit (4 cm) | Great as a "stronger" tier; defer to v2 |
| USB MTP / USB-C accessory | <10 ms | Wired | N/A | Implicit (cable) | Defeats the wireless UX; ignore |
| Bluetooth Classic SPP | 50–200 ms | ~10 m | Yes (paired device) | No | Same relay weakness as BLE; no advantage |

**Conclusion:** **BLE GATT is the right primary transport** because (a) it is universal, (b) Android exposes a system-managed lifecycle for it via CDM, (c) BlueZ on Linux has mature Rust bindings via `bluer`, and (d) the relay risk is bounded by requiring a user gesture per unlock, which we adopt as a non-negotiable.

**PAM/Linux side:**

- `pam-bindings` crate (built on `pam-sys`) is the canonical Rust binding for module authors. PAM itself is a synchronous C ABI; an async tokio runtime must be entered via `Runtime::new()` and `block_on` inside the entry point. The runtime lives for the duration of one `pam_sm_*` call — no persistent state across PAM invocations ([pam crate](https://crates.io/crates/pam)).
- `bluer` is the official BlueZ Rust binding, supports both GATT central and peripheral roles, and presents L2CAP/RFCOMM via a Tokio-shaped API ([bluer docs](https://docs.rs/bluer)).
- BlueZ is reachable from the PAM process: the module runs as root (PAM stacks for `login`/`sudo` invoke modules as root before user transition), so it has access to the system DBus.

**Android side (mirroring prrr architecture):**

- `prrr` uses a workspace layout: top-level Rust core + `prrr-mobile` crate that re-exports a focused API via **UniFFI 0.29**, producing `prrr_mobile.aar` (Rust .so + JNA loader + generated Kotlin bindings) consumed by `prrr-android/` Gradle module ([prrr-mobile/README](file:///home/dmitriy/sources/prrr/prrr-mobile/README.md)).
- The UI is **Jetpack Compose**, minSdk 26, targetSdk 34, Kotlin 1.9, JVM target 17.
- All cross-language types flow through `mobile.udl` (UniFFI IDL). No hand-written JNI.

We will mirror this structure verbatim.

### 2.3 Deep Dives

**Why link-layer relay defeats every BLE crypto scheme.** The NCC attack ([NCC Group advisory](https://www.nccgroup.com/research/technical-advisory-tesla-ble-phone-as-a-key-passive-entry-vulnerable-to-relay-attacks/)) forwards encrypted PDUs at the link layer between two custom radios, never decrypting them. The phone signs whatever challenge it receives; the desktop verifies a signature from "the phone"; both ends are correct individually. Encryption, certificates, signed challenges, and even nonces do not help, because the relay does not need to forge anything — it just lengthens the link. Adding 8 ms of latency is below the noise floor of normal BT-stack jitter, so timing-based detection requires sub-millisecond clocks, which BLE radios do not expose to applications.

The defenses that actually work:
1. **Distance bounding at the physical layer** (UWB, Wi-Fi RTT/FTM): time-of-flight at the speed of light. 30 m of relay path = 100 ns delta, which the radio can measure.
2. **Explicit user interaction on the phone**: turns relay into an attack that requires the user's cooperation, eliminating the passive case. Cost: one tap per unlock.

We choose (2) for v1 (cheap, universal), with (1) as an optional second factor in v2 when hardware permits.

**Why CompanionDeviceService matters on Android.** The naive Android BLE app is killed within minutes of backgrounding ([Android BLE background docs](https://developer.android.com/develop/connectivity/bluetooth/ble/background)). `CompanionDeviceManager.associate()` + `startObservingDevicePresence()` registers a *system-managed* binding: the OS binds `CompanionDeviceService` when the bonded peer appears in BLE range and elevates the process priority above normal background apps. This is the only API that lets a free-floating user app maintain BLE presence indefinitely without the OS reclaiming it ([CompanionDeviceService API](https://developer.android.com/reference/android/companion/CompanionDeviceService)).

**Why the prrr architecture transfers.** prrr already proves that a cdylib Rust crate + UniFFI + Compose works end-to-end for a security-critical mobile feature. Reusing this template means: (a) the auth flow (challenge framing, signing, replay cache) is unit-tested in Rust and runs identically on desktop and phone; (b) only the radio glue is platform-specific; (c) UI code is thin and replaceable.

## 3. Proposal

### 3.1 Approach

```text
┌────────────────────────────────────────────────────────────────────┐
│  Desktop (Linux, this repo)                                        │
│                                                                    │
│   PAM stack ──▶ libpam_syauth.so (cdylib)                          │
│                       │                                            │
│                       ▼                                            │
│             syauth-core (Rust)                                     │
│             • protocol framing & verify                            │
│             • replay nonce cache                                   │
│             • bond keyring access (libsecret)                      │
│                       │                                            │
│                       ▼                                            │
│             syauth-transport (Rust)                                │
│             • BLE GATT central via `bluer`                         │
│             • LAN/mDNS fallback (rustls)                           │
│                                                                    │
│   syauth-cli (binary): pair, list, revoke, status                  │
└────────────────────────────────────────────────────────────────────┘
                              ║   BLE GATT (primary)
                              ║   TLS over LAN (fallback)
                              ▼
┌────────────────────────────────────────────────────────────────────┐
│  Phone (Android companion, mirrors prrr-android)                   │
│                                                                    │
│   Jetpack Compose UI                                               │
│       │                                                            │
│       ▼                                                            │
│   Kotlin glue: CDM, BluetoothGattServer, ForegroundService         │
│       │  (UniFFI generated bindings)                               │
│       ▼                                                            │
│   syauth-mobile (Rust, cdylib via UniFFI)                          │
│   • shared protocol code (same as desktop)                         │
│   • Android Keystore-backed signer                                 │
└────────────────────────────────────────────────────────────────────┘
```

### 3.2 Key Decisions

| # | Decision | Choice | Reasoning | Alternatives considered |
|---|----------|--------|-----------|-------------------------|
| D1 | Primary transport | BLE GATT with CDM | Universal hardware, system-managed background lifecycle on Android, mature Rust support on Linux via `bluer` | Wi-Fi RTT (spotty device support), LAN+TLS (too wide for "presence"), UWB (not ubiquitous), NFC (requires phone-to-laptop contact) |
| D2 | Anti-relay defense | **Mandatory user gesture on phone per unlock** | Cheap, universal, defeats the entire NCC attack class. Costs one tap or biometric per unlock. | UWB/RTT distance bounding (hardware-constrained); passive RSSI threshold (broken); timing-based detection (radio doesn't expose the clocks) |
| D3 | Code-sharing model | UniFFI workspace mirroring prrr | One Rust crate (`syauth-mobile`) re-exports a `mobile.udl`-defined surface to Kotlin; same crate links into `pam_syauth.so`. ≥85% protocol code shared. | Hand-written JNI (brittle, duplicates JNI bookkeeping); Flutter/RN (toolchain mismatch with prrr); pure-Kotlin protocol (forks the wire-format risk) |
| D4 | Crypto suite | Ed25519 (signing) + X25519+ML-KEM-768 (bond key exchange) + ChaCha20-Poly1305 (frame AEAD) | Mirrors prrr's hybrid PQ stack; constant-time prims; rust-crypto + `ml-kem` already in prrr workspace | RSA (slow, big keys); plain X25519 (no PQ hedge); HMAC-only (no asymmetric proof of possession) |
| D5 | Pairing model | LE Secure Connections numeric comparison + out-of-band confirmation in syauth UI (display matching code on both ends) | Defeats MitM-at-pairing per BT spec; the OOB confirmation in our UI is independent of the BT pairing PIN, so an attacker who tricks BT pairing still fails our app-level confirmation | Just Works (MitM-able); QR scan only (loses verification step on the desktop side); typing a passphrase (UX-hostile) |
| D6 | Bond key storage | Linux: kernel keyring via `linux-keyutils` crate, fallback `libsecret`. Android: hardware-backed Android Keystore with `STRONGBOX` when available, `setUserAuthenticationRequired(true)` so the key can only sign when the user has authenticated | Keys never leave secure storage; phone biometric becomes a hardware-enforced gate, not an app-level check | Plaintext `~/.config/syauth/`; environment variable; file with 0600 (all leak on root compromise without any defense) |
| D7 | PAM stack behavior | Module is `auth required` for `sudo` and `gdm-password`; on `PAM_AUTHINFO_UNAVAIL` (peer offline) the stack falls through to `pam_unix.so` (password) which preserves the lockout-recovery story | Lock-out is the worst failure mode; explicit fallback is documented and chosen by the admin, not silent | `auth sufficient` (would weaken the stack); fail-closed only (creates support burden if phone battery dies) |
| D8 | Discovery model | The **desktop** advertises a rotating session-bound UUID; the **phone** scans and connects | Avoids the phone broadcasting a stable identifier (presence-tracking defense); puts the long-lived advertiser on AC power | Phone advertises (drains phone battery, leaks identity); rendezvous through cloud (unwanted dependency) |

### 3.3 ML (Minimum Loveable)

**IN — v0.1.0:**
- `pam_syauth.so` with `pam_sm_authenticate` and `pam_sm_setcred`, returning `PAM_SUCCESS` / `PAM_AUTH_ERR` / `PAM_AUTHINFO_UNAVAIL` correctly.
- `syauth pair` CLI that runs LE Secure Connections numeric comparison and shows a 6-digit OOB confirmation in the terminal.
- `syauth list` and `syauth revoke <peer>` CLI.
- BLE GATT primary transport over `bluer` with a fixed protocol (version 1) of: `[ver:1][nonce:16][payload:?][tag:16]`.
- Android app: one screen showing "Approve unlock for {hostname}?" with two buttons (Approve / Deny) gated by BiometricPrompt. Pairing screen shows the same 6-digit code as the CLI for OOB confirmation.
- Shared `syauth-mobile` crate via UniFFI 0.29, mirroring prrr-mobile layout.
- Documentation: `docs/getting-started.md`, `docs/pam.md`, `docs/security.md`.
- A single e2e test that runs against a mock BLE peer (no real radio) and exercises golden + replay + timeout + revoked.

**OUT — explicitly not in v0.1.0:**
- UWB / Wi-Fi RTT distance bounding (v0.2 candidate, hardware-dependent).
- Multi-peer (one desktop, many phones simultaneously).
- iOS companion (the prrr workspace ships one; we will not, in v0.1).
- Passive/no-tap proximity unlock (we *never* ship this — it's an anti-goal).
- Cloud relay / Internet path.
- Lockscreen "auto-unlock when present" (same anti-goal as passive).
- Per-application policy ("require syauth for sudo only"). The PAM stack already does this via service files; we don't replicate it in syauth.

### 3.4 Anti-Goals

- **No passive unlock, ever.** Every unlock requires a fresh user gesture on the phone. This is the single design decision that lets us ship over BLE without UWB hardware. Removing it would let the project regress into the same class as `pam_blue`/`pam-beacon`/`BLEUnlock`, all of which are broken by relay.
- **No cloud dependency.** Pairing and unlock are LAN/PAN only.
- **No silent fallback to weaker auth.** When syauth is unreachable, PAM returns `PAM_AUTHINFO_UNAVAIL` and the *admin's* configured fallback (typically password) runs — the user knows they fell back.
- **No mutable global state in the PAM module.** Each `pam_sm_*` call is self-contained; the tokio runtime is created and dropped within the call.
- **No `unsafe` Rust outside the documented FFI boundary** (consistent with `AGENTS.md`).

## 4. Technical Design

### 4.1 Architecture

**Workspace layout (mirrors prrr):**

```
syauth/
├── Cargo.toml                     # workspace
├── crates/
│   ├── syauth-core/               # protocol, framing, verification (pure Rust)
│   ├── syauth-transport/          # bluer BLE central + LAN client
│   ├── syauth-pam/                # cdylib → libpam_syauth.so
│   ├── syauth-cli/                # syauth binary (pair / list / revoke / status)
│   └── syauth-mobile/             # cdylib for Android, UniFFI surface
├── syauth-android/                # Gradle, Jetpack Compose UI
│   └── app/
└── docs/, specs/, tests/, Makefile, clippy.toml, rustfmt.toml
```

**Dataflow — unlock path:**

1. PAM invokes `pam_sm_authenticate` (in `syauth-pam` cdylib).
2. The module reads `/etc/syauth.conf`, gets the bond key from the kernel keyring, builds a tokio runtime.
3. `syauth-transport` starts BLE advertising of a rotating session UUID and waits for the bonded phone to connect (deadline configurable, default 1.2 s).
4. On connect, `syauth-core` generates a fresh 16-byte nonce, signs a challenge with the host's Ed25519 key, sends `[ver=1][nonce][challenge][tag=HMAC(bond, ver||nonce||challenge)]`.
5. Phone receives, verifies tag, displays "Approve unlock for `hostname`?" UI via the foreground notification.
6. User taps Approve and authenticates with BiometricPrompt; Android Keystore releases the phone's Ed25519 signing key for one signature; the signed challenge plus a fresh nonce are written back as `[ver=1][nonce'][signature][tag']`.
7. PAM module verifies signature against the bonded phone's public key. If `t_response - t_request < 2.0 s` and signature is valid and nonce' is not in the replay cache: `PAM_SUCCESS`.
8. Module returns; runtime is dropped; no state crosses the FFI boundary.

**Dataflow — pairing:**

1. User runs `syauth pair` on desktop. CLI brings up adapter, requests LE Secure Connections with MITM protection.
2. User opens "Add Computer" in the Android app.
3. BlueZ and Android negotiate LESC pairing. Both display the 6-digit numeric-comparison code.
4. After BT pairing, our **app-level** OOB confirmation kicks in: the CLI shows a *separate* 4-word emoji code derived from `HKDF(bond, "syauth-oob-v1")[0..4]`. The Android app shows the same. User confirms they match (or aborts).
5. Both sides write the bond record to secure storage. Pairing complete.

**Why a second OOB confirmation after BT pairing:** if an attacker compromises the BT pairing (e.g. via a controller-firmware bug or by intercepting numeric comparison via an out-of-band channel), they would also need to spoof the app-level OOB code, which is derived from the freshly-negotiated shared secret. This is defense in depth, cheap to add.

### 4.2 Non-Functional Requirements

- **Performance:**
  - Unlock golden path (peer in BLE range, screen on, user taps within 1 s): total wall-clock < 2.0 s.
  - PAM module memory footprint: < 8 MiB resident.
  - Phone battery cost: < 2% per day at typical use (≈ 50 unlocks).
- **Reliability:**
  - Phone offline / out-of-range: returns `PAM_AUTHINFO_UNAVAIL` within 1.2 s. Never hangs.
  - Kernel suspend/resume: BLE central is restarted on resume; the next unlock attempt succeeds without manual intervention.
  - PAM stack timeout (typically 30 s) is always honored.
- **Security:**
  - Every entry point wrapped in `catch_unwind`; default branch of every match is `PAM_AUTH_ERR`.
  - Bond keys never on disk in plaintext.
  - Replay cache: 64-entry LRU with 10 s TTL.
  - Constant-time crypto via `subtle`.
  - No log line contains key material, nonce, or signature bytes.
- **Observability:**
  - `tracing` spans named `syauth.{pair,unlock,revoke}`; ship to syslog with facility `LOG_AUTHPRIV`.
  - Every PAM return path logs a single line: `syauth: unlock <result> peer=<id> elapsed=<ms> reason=<short>`.
  - `syauth status` prints adapter state, bonded peers, last unlock outcome, last error.

### 4.3 Testing Strategy

- **Unit (`syauth-core`):** protocol framing roundtrip; replay cache eviction; tag computation vector tests against a known-answer test (KAT) file; signature verify positive/negative.
- **Integration (`syauth-pam`):** module is loaded via `pam_start_confdir` against a fixture PAM stack in `tests/pam.d/`; conversation is driven by a custom `pam_conv`; the `syauth-transport` is dependency-injected with an in-process mock peer.
- **E2E (`tests/e2e/`):** real `libpam_syauth.so` + a Python-based mock BlueZ peer over the session DBus; cases (mandatory):
  - golden: ≤ 2 s success
  - peer offline: `PAM_AUTHINFO_UNAVAIL` ≤ 1.2 s
  - peer denies: `PAM_AUTH_ERR`
  - replay (resend prior response): `PAM_AUTH_ERR`
  - bad signature: `PAM_AUTH_ERR`
  - wrong version: `PAM_AUTH_ERR`
  - revoked peer: never goes to radio; `PAM_AUTH_ERR`
  - MTU split frame: reassembled and succeeds
  - panic in core: `catch_unwind` boundary catches it; returns `PAM_AUTH_ERR`, no abort
- **Android:** Compose UI test for the Approve/Deny screen, Robolectric for the BiometricPrompt branch, instrumented test for `CompanionDeviceService` lifecycle.
- **Fuzz:** `cargo fuzz` on the frame parser. Mandatory before v0.1.0.
- **Miri:** on `syauth-core` and `syauth-pam` (pure-Rust portions); ASan on the full cdylib for the e2e suite.

### 4.4 Durability & State

The protocol is **stateless per invocation** by design — there is no multi-step workflow that survives a process restart. The state we *do* persist:

| Item | Where | When written | When read | Recovery |
|------|-------|--------------|-----------|----------|
| Bond record (peer pubkey, name, created_at) | `/var/lib/syauth/bonds.toml` (root-only, 0600) | At pairing completion | At every `pam_sm_authenticate` | Lost = pair again |
| Bond secret key (host's Ed25519) | Kernel keyring; uploaded at boot from `/etc/syauth/host.key` (0600 root) | Once, at install | Once per PAM invocation | Standard backup is the file under `/etc/syauth/host.key` |
| Replay nonce cache | In-memory only | Per-PAM call | Per-PAM call | Discarded with the runtime — acceptable, because the next call gets fresh nonces |
| Revoked peers | `bonds.toml` (status field) | At `syauth revoke` | Per-PAM call | Same as bond record |

Workflow state machine for **pairing** (the only multi-step path):

```
Idle ──[user runs `syauth pair`]──▶ Advertising
Advertising ──[BLE bond + GATT discover]──▶ ProvisionalBonded
ProvisionalBonded ──[OOB code confirmed both sides]──▶ Bonded
ProvisionalBonded ──[timer 60s OR code mismatch]──▶ Revoked
Bonded ──[`syauth revoke`]──▶ Revoked
Bonded ──[≥ N failed unlocks in T window]──▶ Revoked
```

`ProvisionalBonded` is **never** read by the unlock path. Mixing them is the most common bug class in similar projects.

### 4.5 Migration & Compatibility

This is a v0.1.0 product with no prior versions. The wire format carries `version=1` in the first byte; future formats bump it and reject unknown versions explicitly. Roll-forward via a `syauth-cli upgrade` command will be designed in v0.2.

### 4.6 Dependencies

**Linux (Rust):**

| Crate | Version target | Trust note |
|-------|----------------|-----------|
| `pam-bindings` (or `pam-sys`+thin wrapper) | latest stable | Wraps libpam; thin |
| `bluer` | 0.17+ | Official BlueZ bindings; active |
| `tokio` | 1.x | prrr already depends |
| `zbus` | 4.x | DBus client for BlueZ (`bluer` already pulls it transitively) |
| `linux-keyutils` | 0.2+ | Wraps `keyctl(2)`; small surface |
| `secret-service` | 4.x | DBus libsecret client, fallback |
| `chacha20poly1305`, `ed25519-dalek`, `x25519-dalek`, `ml-kem`, `hkdf`, `hmac`, `blake3`, `subtle`, `zeroize` | as in prrr | All pulled from RustCrypto, vetted |
| `serde`, `serde_json`, `toml` | 1.x | Config + bond record |
| `tracing`, `tracing-subscriber` | latest | Logging |
| `clap` | 4.x | CLI |
| `thiserror` | 2.x | Errors |

**Android (Kotlin/Gradle):**

| Lib | Version | Note |
|-----|---------|------|
| Jetpack Compose | as in prrr-android | UI |
| AndroidX Biometric | 1.2+ | BiometricPrompt |
| `net.java.dev.jna:jna:5.x@aar` | as in prrr-android | UniFFI runtime |
| Generated `syauth_mobile.aar` | from UniFFI 0.29 | Our Rust core |

**No new dep that isn't already established in `prrr`'s workspace or the Android sample for CDM.** That keeps the audit surface small.

## 5. User Journey

### 5.1 Persona

**Name:** Alex, Linux power-user, runs Fedora on a desktop and a Pixel 8 on Android 14. Knows `sudo`, `systemctl`, edits PAM stack files comfortably. Wants to stop typing their password 40 times a day for `sudo`, but doesn't trust anything that "just works when phone is nearby."

### 5.2 Trigger

Alex reads about syauth, installs it via `dnf` (or builds from source), runs `syauth pair` while sitting next to the phone with the app already installed.

### 5.3 CJM

**Phase 1 — Install**

- *Intent:* Get syauth onto both devices, ideally in under 5 minutes.
- *Actions:* `sudo dnf install syauth` (or `make install`). On the phone: install APK from F-Droid (planned) or build via Gradle.
- *Pain/Risk:* APK not yet on F-Droid → must sideload; user is uncomfortable with `Settings → Allow from this source`.
- *Success signal:* `syauth --version` works, phone app opens to a pairing screen.

**Phase 2 — Pair**

- *Intent:* Establish a trusted bond between this desktop and this phone.
- *Actions:* `syauth pair` on desktop → BT numeric comparison appears on both → confirm → app-level emoji-code appears on both → confirm → done.
- *Pain/Risk:* BT pairing flake (BlueZ + phone vendor stack); LE Secure Connections fallback to legacy on old adapters; user confirms the wrong device; user is rushed and approves a malicious pairing attempt.
- *Success signal:* `syauth list` shows the phone; phone shows "Paired with `hostname`".

**Phase 3 — Configure PAM**

- *Intent:* Wire syauth into `sudo` so the user can test it.
- *Actions:* Edit `/etc/pam.d/sudo`, insert one line per `docs/pam.md`. Optionally `make verify-pam-config` to check syntax.
- *Pain/Risk:* User edits the file wrong and locks themselves out of `sudo` — classic PAM-config foot-gun.
- *Success signal:* `sudo -k && sudo whoami` triggers a phone notification; tapping Approve runs the command without typing a password.

**Phase 4 — Daily Use**

- *Intent:* `sudo` and screen unlock with one tap on the phone.
- *Actions:* Run a command needing root; pick up phone (which is right there); tap Approve; BiometricPrompt verifies; command runs.
- *Pain/Risk:* Phone is in another room and BLE doesn't reach; phone battery is dead; phone update changed BT permissions and CDM is no longer observing.
- *Success signal:* Median time-to-unlock < 2 s for week-1 measurements.

**Phase 5 — Recovery**

- *Intent:* Get into the system when phone is unavailable.
- *Actions:* Cancel the syauth prompt (or wait for the 1.2 s timeout); the PAM stack falls through to `pam_unix`; type the password as usual.
- *Pain/Risk:* User panics because the prompt looked stuck; password-based fallback was disabled in `/etc/pam.d/sudo`.
- *Success signal:* The fallback works on first try; user is annoyed but not locked out.

**Phase 6 — Revocation**

- *Intent:* Phone is lost or sold; revoke immediately.
- *Actions:* On another machine, ssh to the desktop, `syauth revoke <peer>`. (Or just `rm /var/lib/syauth/bonds.toml` in extremis.)
- *Pain/Risk:* User doesn't notice phone is missing until too late; no remote revoke from the cloud (by design, but worth flagging).
- *Success signal:* `syauth list` no longer shows the peer; subsequent unlock attempts from that peer fail with `revoked` reason.

### 5.4 Friction Map

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| APK distribution (F-Droid takes review time) | 1 | Provide signed APK from GitHub Releases + Play Store track in v0.2 |
| Editing `/etc/pam.d/*` is scary | 3 | Ship `syauth install-pam --service sudo` that does the edit atomically with backup, and `syauth uninstall-pam` to roll back |
| First-time unlock has no feedback while waiting | 4 | Print `[..] waiting for phone` on the terminal during the 1.2 s window |
| User can't tell relay-attack vs honest delay | 4 | Log the RTT to syslog; the phone-side biometric gate makes the question moot for the attacker but reassures the user |
| Recovery flow requires the password the user wanted to stop using | 5 | Document that syauth is *additive*; the password is your recovery key, treat it as such |
| Revoke needs a second machine if phone is the only access | 6 | Document the "physical password fallback" recovery path on the welcome screen |

### 5.5 North Star

A first-time user pairs in under 3 minutes, has `sudo` working with a tap in under 5 minutes, and never gets locked out because the password fallback is on by default. The phone is a key, not a leash: when it's in your hand you tap; when it's not, you type your password.

## 6. Security / Threat Model Summary

(Full treatment goes through `/threat` and lives in `specs/threat/THREAT-{datetime}.md`. This is the executive summary.)

| ID | Threat | Mitigation in v0.1 | Status |
|----|--------|---------------------|--------|
| T-001 | BLE link-layer relay | Mandatory user gesture (biometric) on phone for every unlock | **Mitigated** |
| T-002 | Replay | 16-byte nonce + 64-entry LRU cache, 10 s TTL | **Mitigated** |
| T-003 | MitM during pairing | LE Secure Connections numeric comparison + independent app-level OOB emoji code | **Mitigated** |
| T-004 | Rogue device bonding (user is tricked into pairing) | Pairing must be initiated by `syauth pair` on the desktop; inbound bond requests are not accepted | **Mitigated** |
| T-005 | PAM stack misconfiguration leading to bypass | Ship `syauth install-pam` helper; document `auth required` semantics; recommend keeping `pam_unix` as fallback | **Mitigated by docs + tooling** |
| T-006 | Phone-thief escalation | Phone-side BiometricPrompt with `setUserAuthenticationRequired(true)` on the Keystore signing key | **Mitigated by Android Keystore** |
| T-007 | Root key extraction on Linux | Bond key in kernel keyring; on root compromise the bond is gone anyway. Documented residual risk. | **Accepted residual** |
| T-008 | Denial-of-unlock (BLE jam) | Password fallback in PAM stack by default | **Mitigated** |
| T-009 | Presence inference / tracking | Desktop advertises a rotating UUID; the phone is the scanner (not the advertiser) | **Mitigated** |
| T-010 | Timing side-channel on tag/signature verify | `subtle::ConstantTimeEq` everywhere | **Mitigated** |

## 7. Open Questions

1. **`bluer` peripheral role on older kernels** — does our minimum (BlueZ 5.66+, kernel 5.15+) hold across Debian stable, Fedora 39, Ubuntu 22.04? Verify on each before locking the dep.
2. **Android 14 background BLE** — `CompanionDeviceService` is documented to keep the binding alive, but in practice OEM skins (Xiaomi, OnePlus) are notorious for killing it. Need a tested workaround note ("disable battery optimizations for syauth") in `docs/android-setup.md`.
3. **PAM module argument surface** — minimum is `debug` and `timeout=`. Should we also support `peer=<id>` to restrict the module to one bonded device when multiple are paired? Defer until multi-peer is actually requested.
4. **Multi-peer in v0.2** — when more than one phone is bonded, do we race them all or query a configured priority order? Likely race-with-cap (first valid response wins, with a per-call cap), but flag for review.
5. **iOS port** — explicitly out for v0.1; revisit after Android ships. The same UniFFI surface should work; the platform glue is the Apple Companion API (different from CDM).

## 8. Implementation Roadmap

Suggested phasing (feeds directly into `/roadmap`):

**Phase 0 — Workspace & tooling (1 week)**
- Set up Cargo workspace mirroring prrr (crates/, syauth-android/, scripts/build_aar.sh).
- Wire `make lint`, `make test`, `make bench`, `make android-aar`.
- CI: clippy + fmt + audit + cargo-deny.

**Phase 1 — Protocol core (1–2 weeks)**
- `syauth-core` with framing, replay cache, signature verify, KAT vectors.
- Property tests with proptest for the parser.
- Fuzz target for the parser.
- 100% unit coverage on this crate before moving on.

**Phase 2 — Linux PAM module (1–2 weeks)**
- `syauth-pam` cdylib with the three required entry points.
- Hermetic test rig under `tests/pam.d/`.
- `pamtester`-driven e2e suite, all nine cases from §4.3.
- Run `/ffi` audit before merge.

**Phase 3 — Linux transport (1 week)**
- `syauth-transport` with the `BtPeer` trait, `bluer` impl, in-process mock impl for tests.
- Replace the mock in the PAM e2e tests with the real BLE impl gated behind `SYAUTH_E2E=1`.

**Phase 4 — CLI (3–5 days)**
- `syauth-cli` with `pair`, `list`, `revoke`, `status`, `install-pam`, `uninstall-pam`.

**Phase 5 — Android companion (2–3 weeks)**
- `syauth-mobile` UniFFI crate (≈85% of code already exists in `syauth-core`).
- `syauth-android` Gradle project + Compose UI (one pairing screen, one approve screen, one settings screen).
- `CompanionDeviceService` lifecycle wiring.
- Biometric-gated Keystore signer.

**Phase 6 — Polish & threat-model close-out (1 week)**
- Run `/threat` and resolve every open finding or mark accepted-with-rationale.
- Ship signed APK from GitHub Releases; arrange F-Droid submission.
- Documentation pass on `docs/getting-started.md`, `docs/pam.md`, `docs/security.md`, `docs/android-setup.md`.

**Phase 7 — v0.2 candidates (not in v0.1):**
- UWB / Wi-Fi RTT as optional secondary signal.
- Multi-peer racing.
- iOS port.
- LAN/mDNS fallback transport.

## Sources

- [Apple — Automatically unlock Apple devices](https://support.apple.com/guide/security/automatically-unlock-apple-devices-sec6ab47ebfc/web)
- [NCC Group — Technical Advisory: BLE Phone-as-Key Vulnerable to Relay Attacks](https://research.nccgroup.com/2022/05/15/technical-advisory-tesla-ble-phone-as-a-key-passive-entry-vulnerable-to-relay-attacks/)
- [NCC Group — BLE Proximity Authentication Vulnerable to Relay Attacks (newsroom)](https://www.nccgroup.com/research/technical-advisory-tesla-ble-phone-as-a-key-passive-entry-vulnerable-to-relay-attacks/)
- [Android Developers — CompanionDeviceService](https://developer.android.com/reference/android/companion/CompanionDeviceService)
- [Android Developers — Communicate in the background (BLE)](https://developer.android.com/develop/connectivity/bluetooth/ble/background)
- [Android Developers — Companion device pairing](https://developer.android.com/develop/connectivity/bluetooth/companion-device-pairing)
- [Android Developers — Wi-Fi RTT (802.11mc / 802.11az)](https://developer.android.com/develop/connectivity/wifi/wifi-rtt)
- [bluer — Official BlueZ Rust bindings](https://github.com/bluez/bluer)
- [pam-sys on crates.io](https://crates.io/crates/pam-sys)
- [pam on crates.io](https://crates.io/crates/pam)
- [pam-bluetooth (l3pp4rd, FOSS reference)](https://github.com/l3pp4rd/pam_bluetooth)
- [pam-beacon (FOSS reference)](https://github.com/muesli/pam-beacon)
- [BLEUnlock (macOS, FOSS reference)](https://github.com/ts1/BLEUnlock)
- [KDE Connect (architecture reference)](https://en.wikipedia.org/wiki/KDE_Connect)
- [prrr-mobile/README.md (sibling-repo architecture template)](file:///home/dmitriy/sources/prrr/prrr-mobile/README.md)
- [prrr-android/app/build.gradle.kts (sibling-repo Gradle template)](file:///home/dmitriy/sources/prrr/prrr-android/app/build.gradle.kts)
