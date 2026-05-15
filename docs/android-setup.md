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

## Future setup steps

The following will be appended as future roadmap items land:

- **S-018** — disable battery optimization for syauth, register
  CompanionDeviceManager association, notification channel setup.
- **S-019** — pairing recovery (re-bond after factory reset).
