# syauth Android setup

Roadmap items that contribute to this document:

- **S-015** — Gradle / Compose scaffold (the `make android-aar` /
  `make android-test` workflow).
- **S-017** — Approve screen + BiometricPrompt + Keystore signer.
- **S-018** (future) — `CompanionDeviceService` + battery-optimization
  setup steps.

## Building and running

The companion app lives under `syauth-android/`. Build the AAR first
(`make android-aar`, requires the Android NDK), then run instrumented
tests against an emulator with `make android-test`. Both targets skip
cleanly on hosts that lack the toolchain, with an actionable message.

## Keystore key parameters (S-017)

The Approve screen gates each signing operation on a fresh user gesture
via the Android Keystore. The key parameters were chosen for maximum
portability across supported devices (minSdk 26 / API 26+).

### Curve choice — secp256r1 (EC P-256), not Ed25519

The S-017 DoD calls for `KeyProperties.KEY_ALGORITHM_EC` with curve
`secp256r1` and a documented fallback. Ed25519 became a first-class
Android Keystore algorithm only in API 33 and is not present on every
Android 13 device, while EC P-256 (secp256r1) has been supported since
API 23. The gate key is therefore generated unconditionally as
`KEY_ALGORITHM_EC` with `ECGenParameterSpec("secp256r1")` and the
JCA signature algorithm `SHA256withECDSA`. The wire-protocol signature
(the 64-byte Ed25519 signature the desktop verifies) is produced by the
Rust core through the UniFFI `signChallengeResponse(seed, frame)`
surface; it is a separate concern from the Keystore-backed gate.

The Keystore gate signature is logged for audit but **not** sent on the
wire. Its purpose is to fail loudly if the Keystore disagrees with the
`BiometricPrompt` callback — i.e., the OS reported "biometric success"
but the Keystore refused to release the private key. That mismatch
would be a security-relevant anomaly worth observing.

### Authentication requirements

The key is built with:

```kotlin
KeyGenParameterSpec.Builder(alias, PURPOSE_SIGN or PURPOSE_VERIFY)
    .setAlgorithmParameterSpec(ECGenParameterSpec("secp256r1"))
    .setDigests(KeyProperties.DIGEST_SHA256)
    .setUserAuthenticationRequired(true)
    .setUnlockedDeviceRequired(true)      // API 28+
    .setIsStrongBoxBacked(true)           // API 28+; try/catch
```

`setUserAuthenticationRequired(true)` is the hardware-enforced gate:
without a fresh `BiometricPrompt` unlock that bound the `Signature` to
this key via `CryptoObject(signature)`, the `sign()` call raises
`UserNotAuthenticatedException`. `setUnlockedDeviceRequired(true)`
(API 28+) prevents use of the key while the device is in the locked
state; older hardware falls through silently.

### StrongBox try / fallback

StrongBox (a discrete tamper-resistant element) is requested via
`setIsStrongBoxBacked(true)`. On older or non-Pixel hardware the
`KeyGenParameterSpec.Builder.build()` call throws
`StrongBoxUnavailableException`; we catch it and retry with
`setIsStrongBoxBacked(false)`. The resulting `KeyInfo` records the
boolean so the audit log (S-018) can capture whether StrongBox was in
fact used.

### BiometricPrompt allowed authenticators

The prompt is built with
`BIOMETRIC_STRONG | DEVICE_CREDENTIAL`:

- `BIOMETRIC_STRONG` (Class 3 biometric) is required to bind a
  `CryptoObject` to a Keystore signing key — Class 2 (weak)
  biometrics cannot.
- `DEVICE_CREDENTIAL` (PIN / pattern / password) gives users without
  enrolled biometrics a fallback rather than failing outright.

If neither modality is available (no biometric and no device
credential), the presenter resolves to
`BiometricResult.Unavailable` and the ViewModel emits
`Denied(BiometricUnavailable)`; the desktop sees a `PeerDenied`
frame.

### Ed25519 seed handling (transitional)

The wire-protocol Ed25519 seed currently lives behind a
`SigningKeyProvider` interface that returns the 32-byte bytes to the
ViewModel, which then hands them to UniFFI. This is the one
production code path where Ed25519 key bytes briefly cross the
Kotlin boundary — a known gap relative to the strict reading of the
S-017 DoD line "the crypto code never sees the private key bytes."

The Rust crypto core (the "core" in that DoD line) never sees the seed
in plaintext storage; it receives it as a function argument from
UniFFI. The seed is loaded from a Keystore-encrypted file at app start
and lives in a `ByteArray` for the lifetime of the ViewModel; the
follow-up (tracked alongside S-018's background bridge) tightens this
to use a Keystore-wrapped seed via a `Cipher`-init flow once the
Keystore's Ed25519 support stabilizes on the targeted device range.

## Battery-optimization exclusion (S-018)

`SyauthCompanionService` is bound by the OS only while the bonded
peer is observed in BLE range. Without an explicit battery-optimization
exclusion the OS will doze the app within minutes, and the binding will
quietly stop being delivered. The setup is one tap from the user once,
and the app pops the system dialog on first launch (and again after a
fresh CDM association).

### Why the deep-link

`Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` was introduced
in API 23 (doze, Marshmallow). API 30 (R) tightened the doze rules
significantly and added the App Standby Buckets that progressively
restrict our binding the longer the app sits idle. API 33 added
`POST_NOTIFICATIONS` as a runtime grant — the user must accept the
notification permission separately from battery optimisation. API 34
introduced the `connectedDevice` foreground-service sub-type the
manifest declares; without it our service cannot stay foregrounded
while bound on Android 14+.

The deep-link util lives at
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/BatteryOptimizationDeepLink.kt`
as `batteryOptimizationDeepLinkIntent(context)`. It returns the system
intent that opens the per-app battery optimization dialog;
`isIgnoringBatteryOptimizations(context)` returns whether the
exclusion is already granted.

### When the app should prompt

1. **First launch** — the home screen calls
   `isIgnoringBatteryOptimizations` and, if false, fires the deep-link
   immediately.
2. **After a fresh CDM association** — the pair-complete state
   transitions to `Bonded`, then the home screen re-runs the check on
   re-entry. This catches the case where the user accepted CDM but
   skipped (or revoked) the battery prompt.

### OEM-specific notes

Xiaomi MIUI, OnePlus, Vivo, and Oppo skins are notorious for ignoring
the CDM contract — they apply additional kill rules on top of the OS
defaults. The mitigation is the same exclusion plus a per-OEM
"autostart" toggle that lives outside the public Android API. We
document the known workarounds in the README's troubleshooting section
once the test rack covers each skin; for now the SPEC §7 open
question 2 tracks this gap.

## Companion-device association lifecycle (S-018)

The S-018 flow registers the bonded computer with
`CompanionDeviceManager.associate()` so the OS will wake the app via
`CompanionDeviceService` whenever the peer comes into BLE range.

### When the association is requested

- **At pair-complete (S-016 happy path).** The
  `PairingViewModel` calls
  `companionAssociator.associate(peer)` immediately after
  `bondPersister.persist(record)` succeeds and **before**
  transitioning to `Bonded(name)`. The user sees two consecutive OS
  prompts during pairing:
  1. The BT pairing numeric-comparison dialog (LESC).
  2. The CDM "syauth wants to remember this companion device" dialog.
- **On user revocation.** If the user removes the pair via system
  settings, the OS stops binding the service. Re-establishing the
  binding requires re-pairing — there is no resurrect-without-pair
  path in v0.1.

### Service-binding lifecycle

- The OS observes the bonded peer in BLE range via its native
  scanner; it does not consult `BLUETOOTH_SCAN` ours.
- On peer-in-range it binds `SyauthCompanionService` (the manifest
  `CompanionDeviceService` subclass) and calls
  `onDeviceAppeared(AssociationInfo)`.
- Our service opens a `BluetoothGattServer` via
  `BluerlessGattServerController` and registers two characteristics
  (challenge / response) under `SYAUTH_GATT_SERVICE_UUID`.
- On peer-out-of-range the OS calls
  `onDeviceDisappeared(AssociationInfo)`; we close the GATT server.

### Why the GATT server is short-lived

Keeping a long-lived foreground service draining the radio is exactly
what `CompanionDeviceService` exists to avoid. The OS owns the
lifecycle; we own only the GATT setup/teardown within the binding's
lifetime. The result: zero radio usage when the bonded peer is out of
range, and zero process-keepalive battery cost.

## Provision-file bootstrap (v0.1 demo)

For v0.1 the LESC pair flow is stubbed on both ends: the desktop's
phone-side BLE backend and the phone's `StubPairBackend` both return
"real-radio path lands in a future roadmap item". To still deliver a
working end-to-end unlock, the desktop's `syauth provision-test`
subcommand generates all shared material out of band and emits a
single TOML package (`syauth-provision.toml`) carrying the 32-byte
`bond_key`, the phone's Ed25519 signing-key seed, and the bond
metadata. The operator transports this file to the phone over a USB
cable (`adb push syauth-provision.toml /sdcard/Download/`). On first
launch the phone reads it, persists the bond to app-private storage,
and deletes the source file from `Downloads/` so the plaintext key
does not linger in shared storage. Subsequent launches read the
persisted record directly; the provision file is consumed exactly
once per install.

The Ed25519 seed is stored in plaintext under `context.filesDir`
(specifically `syauth-bond.toml` — the same shape as the provision
file). This is acceptable for v0.1 because the directory is sandboxed
to the app's UID and the device threat model assumes an unrooted
device, but it is **not** the long-term design. The canonical v0.2
path will wrap the seed in an Android-Keystore-backed AES cipher and
require a fresh BiometricPrompt gesture to decrypt; the
`SigningKeyProvider` seam in `approve/SigningKeyProvider.kt` exists
precisely so this swap is local. The provision package itself
contains a private key and must NEVER be transported over a network
or shared storage — only over a trusted USB cable under the
operator's physical control.

## Future setup steps

The following will be appended as future roadmap items land:

- **S-019** — pairing recovery (re-bond after factory reset).
