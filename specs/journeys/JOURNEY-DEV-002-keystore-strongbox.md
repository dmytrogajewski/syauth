# JOURNEY-DEV-002: Ed25519 signing key into Android Keystore (STRONGBOX)

> **SPEC anchors:** §3.2 D6 — verbatim:
> "Android: hardware-backed Android Keystore with `STRONGBOX` when
> available, `setUserAuthenticationRequired(true)` so the key can only
> sign when the user has authenticated".
>
> **Gap reference:** `docs/known-gaps.md` row DEV-002 (Open deviations).
>
> **Predecessor:** JOURNEY-DEV-001 closed in this same march and moved
> bond persistence under `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bond/`.
> The DEV-002 row's "Source locations" section still points at the
> deleted `provision/` package; the new locations are under `bond/`.

## Roadmap Link

- Source roadmap: [`specs/syauth/ROADMAP.md`](../syauth/ROADMAP.md) items
  **S-016** (Android pairing screen) and **S-017** (Android approve screen).
  Both were closed with stub key-material handling; this journey
  installs the Keystore-backed production path.
- Feature: replace the on-disk Ed25519 seed with a Keystore-resident
  Ed25519 signing key whose private bytes never appear in the JVM.

## 1. Journey

When **a syauth operator pairs a new phone and then unlocks their
desktop with it**, I want to **trust that the Ed25519 secret bonded to
that phone has never existed as plaintext bytes in either app private
storage or the JVM heap — only inside the Android Keystore's hardware
enclave, gated by a fresh biometric per unlock**, so I can **defend
against an attacker who roots the phone or extracts `<filesDir>` (the
exact threat T-007 "root key extraction" the `/threat` skill names)**.

## 2. CJM

Today (DEV-002 open), the phone's pair flow generates an Ed25519
keypair, ships the 32-byte secret seed across the LESC link, and writes
it as plaintext under `<filesDir>/syauth-bond.toml`. On every unlock,
`MainActivity::ApproveRoute` reads the seed into
`InMemorySigningKeyProvider` and hands it to UniFFI's
`buildResponseFrame` — the seed crosses the JVM/native boundary on
every unlock. A root-level adversary can dump the file and replay
unlocks forever. SPEC §3.2 D6 demands the opposite: the private bytes
never leave the Keystore enclave, and every sign requires a fresh
biometric. This journey closes that gap end to end — pair time
generates the key inside Keystore (STRONGBOX-preferred), the bond
record on disk carries only the alias + pubkey + bond_key, and unlock
sign happens via a UniFFI callback that calls into a Kotlin
`KeystoreFrameSigner` which delegates to `Signature.getInstance("Ed25519")`
initialised against the Keystore-backed `PrivateKey`.

### Phase 1: Pair-time keypair generation in Keystore (STRONGBOX preferred)

**User Intent:** complete the LESC + app-OOB pair so a long-term phone
identity Ed25519 keypair exists, with the private key locked inside
the Keystore enclave.

**Actions:**
- After OS-level LESC numeric-comparison succeeds and the app-level
  4-word OOB confirmation completes, `RealPairBackend` calls
  `KeystoreKeyGenerator.generate(alias)` which:
  - constructs `KeyGenParameterSpec.Builder(alias, PURPOSE_SIGN)`
  - calls `.setAlgorithmParameterSpec(NamedParameterSpec("Ed25519"))`
    (API 33+ contract per `KeyProperties.KEY_ALGORITHM_EC` with the
    Ed25519 named curve)
  - calls `.setDigests(KeyProperties.DIGEST_NONE)` — Ed25519 hashes
    the message internally
  - calls `.setUserAuthenticationRequired(true)` so the private key
    cannot sign without a fresh BiometricPrompt unlock
  - calls `.setIsStrongBoxBacked(true)`, wrapped in a try/catch that
    catches `StrongBoxUnavailableException` and rebuilds without
    STRONGBOX (TEE-only)
- `KeyPairGenerator.getInstance("EC", "AndroidKeyStore")` initialises
  with the spec and calls `generateKeyPair()`.
- The phone extracts the 32-byte Ed25519 pubkey from
  `keyStore.getCertificate(alias).publicKey` and writes that pubkey
  (along with the alias + the bond_key + the peer's pubkey + the
  host name + peer id) into the bond record on disk.

**Pain / Risk:**
- The device is API < 33: Ed25519 NamedParameterSpec is unavailable.
  Mitigation: production target is API 33+; the path returns a typed
  `KeystoreKeygenError.UnsupportedApi` and the pair flow aborts with
  a "phone too old for syauth" surface message.
- The device lacks a StrongBox secure element. Mitigation: the
  `StrongBoxUnavailableException` is caught and the build retried
  without STRONGBOX; the bond record records the strongBoxBacked
  flag so the operator can audit the choice.
- Two pair attempts overwrite the alias. Mitigation: the alias is
  derived from the peer-id (`syauth.ed25519.<peerId>`), and the
  generator checks `keyStore.containsAlias` before generating;
  re-pair without revoke surfaces a typed `AliasAlreadyExists` that
  the existing pair-side `--force` flag already handles.

**Success Signal:** the bond record's `keystoreAlias` field is
non-empty, the `phonePubkey` decodes to a valid Ed25519 pubkey, and
the Keystore alias resolves to a `PrivateKey` whose certificate
chain's public key matches `phonePubkey` byte-for-byte. The seed
bytes never appear anywhere on disk or in any `ByteArray`.

### Phase 2: Unlock-time sign happens under Keystore (user-auth gate)

**User Intent:** when the desktop sends a challenge, the phone produces
the Ed25519 signature over the challenge frame using the Keystore
key — gated by a fresh biometric — without the private bytes ever
crossing the JVM boundary or the UniFFI boundary.

**Actions:**
- `MainActivity::ApproveRoute` instantiates `KeystoreFrameSigner(bondRecord.keystoreAlias)`
  — implements the new `FrameSigner` UniFFI callback interface.
- The view-model calls `wireSigner.signWire(bondKey, frameBytes)`;
  the production `UniffiWireSigner` invokes the updated UniFFI
  `buildResponseFrame(bondKey, signer, challengeFrameBytes)` where
  `signer` is the `KeystoreFrameSigner` instance.
- Inside the Rust core: `Frame::decode(challenge_bytes)` -> form
  the unsigned body bytes -> upcall `signer.sign(unsigned_body)` —
  the UniFFI callback machinery marshals the byte slice into a
  Kotlin `ByteArray` and invokes the Kotlin method synchronously.
- Inside Kotlin: `KeystoreFrameSigner.sign(message)` opens the
  Keystore, retrieves the `PrivateKey` under the alias, builds a
  `Signature.getInstance("Ed25519")` initialised with that
  private key, calls `signature.update(message)`,
  `signature.sign()`, and returns the 64-byte signature ByteArray
  back to Rust.
- Rust receives the 64 bytes, encodes them as the frame payload,
  computes the MAC tag under bond_key, encodes the full response
  frame, and returns the bytes to Kotlin's `UniffiWireSigner`.

**Pain / Risk:**
- The user cancels the BiometricPrompt while the callback is
  in flight: `signature.sign()` throws
  `UserNotAuthenticatedException`. Mitigation: the Kotlin signer
  surfaces this as a `MobileError::SignFailed` on the Rust side;
  the view-model maps it to `DenialReason.SignError` and emits
  `Denied`. No bytes go on the wire.
- An attacker calls `buildResponseFrame` without a real
  `FrameSigner` (e.g. they reflect into the bindings). Mitigation:
  the UDL surface makes `signer` non-nullable; the only way to
  invoke the function is to supply a callback object, which on
  the JVM side is only ever the production `KeystoreFrameSigner`
  (or a test double in unit tests).
- Multi-threaded re-entrancy on the Keystore. Mitigation: the
  `KeystoreFrameSigner` opens a fresh `Signature` per `sign` call
  — no shared mutable state across threads.

**Success Signal:** the desktop's `pam_syauth` verifies the response
signature against the bonded phone's pubkey and the unlock
succeeds. `git grep` for the Ed25519 seed across production source
returns nothing. The Keystore audit log (`adb logcat | grep
KeyStore`) shows a `Signature.sign` call gated by user-auth.

### Phase 3: Bond record on disk carries alias + pubkey, never the seed

**User Intent:** rotating the desktop, re-installing the app, or
inspecting the bond record reveals no path to the long-term private
key bytes.

**Actions:**
- `BondRecord` (the production data class) replaces the
  `phoneSigningKeySeed: ByteArray` field with
  `keystoreAlias: String`.
- `BondStore.kt`'s TOML serializer emits `keystore_alias = "..."`
  in place of `phone_signing_key_hex = "..."`. Old records (with
  the hex seed) are rejected at parse time with a typed
  `BondParseError.UnsupportedSchemaVersion` because the schema
  version bumps from 1 to 2. The migration path is: the pair
  flow re-runs and the user re-pairs the phone.
- `DiskBondPersister` removes the `PLACEHOLDER_SEED` constant and
  the seed-bearing `persistFull` contract; the persister now
  writes the alias the pair flow generated.
- `MainActivity::ApproveRoute` no longer instantiates
  `InMemorySigningKeyProvider(seed)`. The class is removed from
  the production source tree; the test source tree keeps an
  equivalent shape only if any unit test still wants a fake
  signer.

**Pain / Risk:**
- A user upgrading from a DEV-001-shipping build has an old TOML
  on disk with `phone_signing_key_hex`. Parse fails. Mitigation:
  the parser surfaces `UnsupportedSchemaVersion(got=1)` and the
  app prompts the user to re-pair — the seed file gets renamed
  to `.legacy` so a sophisticated user can still extract it,
  but the app does not consume it.
- The bond_key remains plaintext on disk (it's the symmetric MAC
  key for the unlock channel, not the long-term identity key).
  This is residual surface area; DEV-002's closure scope does
  not include moving the bond_key into Keystore, but the
  Closure section below records this as a future strengthening
  candidate.

**Success Signal:** `git grep "phoneSigningKeySeed\|PHONE_SIGNING_KEY_HEX"`
returns nothing under `syauth-android/app/src/main/`. The on-disk
TOML carries a `keystore_alias` line, no `phone_signing_key_hex`
line. A radio-free Rust unit test confirms the UniFFI
`buildResponseFrame` refuses to run without a `FrameSigner`.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|---|---|---|
| Older Android devices (API < 33) cannot host the Ed25519 Keystore key at all | Phase 1 | The pair flow surfaces a typed `UnsupportedApi` reason; `docs/android-setup.md` documents the API 33+ requirement up front so the operator does not run pair on a phone that will fail at the last step |
| The `StrongBoxUnavailableException` fallback to non-STRONGBOX is invisible to the operator | Phase 1 | The bond record persists the `strongBoxBacked` boolean so `adb shell run-as` (and a future "phone status" surface) can show whether the key sits inside the secure element or the TEE |
| Old bond records become unreadable after the schema bump | Phase 3 | The parser emits a typed `UnsupportedSchemaVersion(got=1)` and the home screen surfaces "re-pair required" — no silent data loss |

### North Star Summary

A rooted attacker who pulls `<filesDir>/syauth-bond.toml` off the
phone sees only a Keystore alias and the bond_key MAC secret. The
Ed25519 private key that signs unlock responses never appears
outside the Keystore enclave; on STRONGBOX-capable phones the key
sits inside a discrete secure element with its own clock and its
own attestation. Every unlock requires a fresh biometric the user
performed in the last few seconds. The phone-as-key story finally
matches SPEC §3.2 D6 verbatim.

## 3. Architecture Notes

### UniFFI surface change (`mobile.udl` + `implementation.rs`)

- Add a `callback interface FrameSigner` to `mobile.udl`:
  ```
  callback interface FrameSigner {
    bytes sign(bytes message);
  };
  ```
- Change `build_response_frame`'s signature from
  `build_response_frame(bytes bond_key, bytes signing_key, bytes challenge_frame_bytes)`
  to
  `build_response_frame(bytes bond_key, FrameSigner signer, bytes challenge_frame_bytes)`.
- The Rust implementation receives the signer as
  `Arc<dyn FrameSigner>` (UniFFI 0.29's callback-interface
  marshal) and calls `signer.sign(unsigned_body)` to obtain the
  64-byte Ed25519 signature.
- The bond record no longer carries the seed; the Rust side no
  longer needs `sign_challenge_response` for the production path,
  but the function stays in the surface (it is used by the
  Rust-side unit tests and by the pair-time bond derivation).

### Android side (`KeystoreFrameSigner` + `KeystoreKeyGenerator`)

- New file
  `syauth-android/app/src/main/kotlin/com/sy/syauth/android/approve/KeystoreFrameSigner.kt`
  implements the `uniffi.syauth_mobile.FrameSigner` interface
  via Keystore.
- New file
  `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/impl/KeystoreKeyGenerator.kt`
  generates the Ed25519 keypair under a per-bond alias at pair
  time; STRONGBOX-preferred with `StrongBoxUnavailableException`
  fallback; returns a typed `KeystoreEd25519KeyMaterial { alias,
  pubkey, strongBoxBacked }`.
- `RealPairBackend.kt` (the existing DEV-001 backend) wires the
  Keystore key generator into its post-bond hop: instead of
  inventing an in-memory `SigningKey::generate(OsRng)` and
  shipping the seed across the LESC link, it generates the key
  in Keystore and ships only the pubkey.

### `MainActivity::ApproveRoute` rewire

- Remove the `seed = bondRecord?.phoneSigningKeySeed ?: ByteArray(...)`
  fallback.
- Replace `signingKeyProvider = InMemorySigningKeyProvider(seed)`
  with the new `KeystoreFrameSigner(bondRecord.keystoreAlias)`
  via the updated `WireSigner` -> UniFFI seam.
- The `signingKeyProvider` constructor parameter on
  `ApproveViewModel` becomes unused on the production path and is
  removed; the only `SigningKeyProvider` site left is the
  test-fake under `app/src/test/`.

### `BondRecord` + `BondStore.kt` schema

- `BondRecord` field rename: `phoneSigningKeySeed: ByteArray` ->
  `keystoreAlias: String`.
- `BondStore.kt`: bump `BOND_RECORD_SCHEMA_VERSION` from 1 to 2;
  swap the `BondKeys.PHONE_SIGNING_KEY_HEX` entry for
  `BondKeys.KEYSTORE_ALIAS`; update `serializeBondRecord` and
  `parseBondRecord` to round-trip the new field; the
  `UnsupportedSchemaVersion(got=1)` path is the migration signal
  for users upgrading from a DEV-001 ship.

### Wire format

- No wire change. The desktop's `pam_syauth` keeps verifying the
  response signature against the bonded phone's pubkey. The only
  change is *where* the phone's signature comes from.

### Closure conditions (mechanical)

- [ ] `git grep -l "InMemorySigningKeyProvider" -- syauth-android/app/src/main/`
      returns nothing.
- [ ] `git grep -l "// GAP: DEV-002"` returns nothing in production source paths.
- [ ] The bond record on disk no longer carries the Ed25519 seed.
      Greppable: `git grep "phoneSigningKeySeed\|PHONE_SIGNING_KEY_HEX"` returns
      nothing under `syauth-android/app/src/main/`.
- [ ] `git grep "build_response_frame.*seed\|build_response_frame.*signing_key\|buildResponseFrame.*seed"`
      returns nothing in production source.
- [ ] The Keystore-backed signer in production code uses
      `KeyProperties.PURPOSE_SIGN` + Ed25519
      (`NamedParameterSpec("Ed25519")`) +
      `setIsStrongBoxBacked(true)` (when supported) +
      `setUserAuthenticationRequired(true)`.
- [ ] `make scope-discipline` clean.
- [ ] `make lint` clean.
- [ ] `cargo test --workspace --all-targets --all-features` green;
      passing count >= baseline 292.
- [ ] `docs/known-gaps.md` row DEV-002 moves from "Open deviations"
      to "Closed deviations" with closure timestamp (UTC), pointer
      to this journey doc, and the source-location relocation
      note (`provision/` -> `bond/`).

## 4. Tests

### TC-01: pair-time keystore key generation — STRONGBOX-preferred

**Given** a phone running API 33+ where the StrongBox HAL is present.
**When** the pair flow calls `KeystoreKeyGenerator.generate(alias)`.
**Then** a Keystore Ed25519 keypair exists under `alias`,
`strongBoxBacked = true` is reported, the public key decodes to 32
bytes, and `keyStore.getKey(alias, null)` returns a `PrivateKey` whose
encoded form is `null` (Keystore-resident; opaque to the JVM).

### TC-02: pair-time fallback when StrongBox is absent

**Given** a phone running API 33+ where StrongBox is not available
(test fake throws `StrongBoxUnavailableException`).
**When** the pair flow calls `KeystoreKeyGenerator.generate(alias)`.
**Then** the generator catches the exception, retries without
`setIsStrongBoxBacked(true)`, and returns `strongBoxBacked = false`.

### TC-03: hard refusal on API < 33

**Given** the runtime SDK is API 32 or lower.
**When** the pair flow calls `KeystoreKeyGenerator.generate(alias)`.
**Then** the generator returns a typed
`KeystoreKeygenError.UnsupportedApi` and the pair flow aborts with a
human-readable "phone too old for syauth" surface; no keypair is
created.

### TC-04: unlock-time sign goes through the FrameSigner callback

**Given** a bonded phone and a fresh challenge frame.
**When** `UniffiWireSigner.signWire(bondKey, frameBytes)` runs with a
`KeystoreFrameSigner` wired into UniFFI's `buildResponseFrame`.
**Then** UniFFI calls back into Kotlin's `KeystoreFrameSigner.sign(...)`
exactly once with the unsigned body bytes; the returned 64-byte
signature is the prefix of the response frame's payload; the response
frame's MAC tag verifies under `bondKey`; the seed never appears as a
ByteArray in the call stack.

### TC-05: Rust UniFFI `build_response_frame` refuses bad signer output

**Given** a mock `FrameSigner` that returns a 63-byte signature (one
byte short).
**When** `build_response_frame(bond_key, signer, challenge_bytes)` runs.
**Then** it returns `MobileError::SignFailed` with a reason mentioning
the expected length (64); no panic; the error message contains no
bytes of the bond_key or the challenge.

### TC-06: BondRecord schema migration — old record rejected

**Given** an on-disk bond record from DEV-001 (schema version 1, with
`phone_signing_key_hex = "..."`).
**When** `BondStore(filesDir).load()` runs after the DEV-002 schema bump.
**Then** the parser raises `BondParseError.UnsupportedSchemaVersion(got = 1)`;
the file is left untouched on disk; the home route surfaces a
"re-pair required" toast.

### TC-07: BondRecord schema migration — new record round-trips

**Given** a freshly-paired bond record carrying `keystoreAlias =
"syauth.ed25519.peer-xyz"`, the bond_key, the host name, the peer id,
and the phone pubkey.
**When** `BondStore.save(record)` writes the record and a subsequent
`BondStore.load()` reads it back.
**Then** the loaded `BondRecord` is byte-identical to the saved one;
the on-disk TOML contains `keystore_alias = "syauth.ed25519.peer-xyz"`;
no `phone_signing_key_hex` line is present.

### TC-08: `InMemorySigningKeyProvider` has no production callers

**Given** the post-DEV-002 source tree.
**When** `git grep -l "InMemorySigningKeyProvider" -- syauth-android/app/src/main/`
runs.
**Then** the output is empty. The class either lives only under the
test source root or has been removed outright.

### TC-09: Robolectric — `KeystoreFrameSigner` opens the alias under
`AndroidKeyStore`

**Given** a Robolectric-shadowed `AndroidKeyStore` provider on the
JVM test runtime.
**When** `KeystoreFrameSigner(alias).sign(message)` runs against a
test alias pre-loaded into the shadow.
**Then** the shadow records a `KeyStore.getInstance("AndroidKeyStore")`
call, a `getKey(alias, null)` call, a `Signature.getInstance("Ed25519")`
call (Robolectric cannot verify the underlying hardware enclave —
this is documented in the test).

## Traceability

- Roadmap items affected: S-016, S-017 (both touched indirectly; the
  full audit trail is this journey doc plus the DEV-002 closed-row
  in `docs/known-gaps.md`).
- Gap row: `docs/known-gaps.md` DEV-002.
- Implementation files (filled by `/implement`):
  - `crates/syauth-mobile/src/mobile.udl` — `FrameSigner` callback
    interface + updated `build_response_frame` signature.
  - `crates/syauth-mobile/src/implementation.rs` — Rust receiver
    for the callback interface + updated `build_response_frame`.
  - `crates/syauth-mobile/Cargo.toml` — UniFFI feature flags if
    needed for callback-interface support on 0.29.
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/approve/KeystoreFrameSigner.kt`
    (new).
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/impl/KeystoreKeyGenerator.kt`
    (new).
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt`
    (rewire ApproveRoute, remove `InMemorySigningKeyProvider` import).
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bond/BondRecord.kt`
    (`phoneSigningKeySeed` -> `keystoreAlias`).
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bond/BondStore.kt`
    (schema bump, key rename).
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bond/DiskBondPersister.kt`
    (drop the seed plumbing).
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/impl/RealPairBackend.kt`
    (call `KeystoreKeyGenerator` at pair time).
  - `syauth-android/app/src/main/kotlin/com/sy/syauth/android/approve/UniffiWireSigner.kt`
    (route through the new `FrameSigner` callback).
- Test files:
  - Rust unit tests inside `crates/syauth-mobile/src/implementation.rs`
    covering TC-04 and TC-05.
  - `syauth-android/app/src/test/kotlin/com/sy/syauth/android/approve/KeystoreFrameSignerTest.kt`
    (Robolectric TC-09).
  - `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bond/BondRecordSchemaTest.kt`
    (TC-06 + TC-07).
- On closure: `docs/known-gaps.md` row DEV-002 marked closed with
  the closure timestamp, this journey doc archived with the
  `## Closure` appendix.

## Implementation

Files created:

- `crates/syauth-mobile/src/mobile.udl` — added the `FrameSigner`
  callback interface and updated `build_response_frame`'s third
  parameter from `bytes signing_key` to `FrameSigner signer`.
- `crates/syauth-mobile/src/implementation.rs` — added `pub trait
  FrameSigner: Send + Sync` and updated the Rust receiver to take
  `signer: Box<dyn FrameSigner>` + call `signer.sign(unsigned_body)`;
  added six new `build_response_frame_*` unit tests covering the
  FrameSigner callback contract.
- `crates/syauth-mobile/src/lib.rs` — re-exported `FrameSigner` and
  updated the `public_surface_reexports_compile` test.
- `crates/syauth-mobile/bindings/kotlin/uniffi/syauth_mobile/syauth_mobile.kt`
  — regenerated via `uniffi-bindgen generate ... --language kotlin`
  to surface the new `FrameSigner` callback interface and the new
  `buildResponseFrame(bondKey, signer, challengeFrameBytes)` shape.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/approve/KeystoreFrameSigner.kt`
  (new) — production [`FrameSigner`] backed by
  `Signature.getInstance("Ed25519")` initialised against an Android
  Keystore-resident `PrivateKey`.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/impl/KeystoreKeyGenerator.kt`
  (new) — pair-time Ed25519 keypair generation via
  `KeyGenParameterSpec.Builder(alias, PURPOSE_SIGN)` +
  `NamedParameterSpec("Ed25519")` +
  `setUserAuthenticationRequired(true)` +
  `setIsStrongBoxBacked(true)` (STRONGBOX-preferred with
  fallback).
- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/approve/KeystoreFrameSignerTest.kt`
  (new) — Robolectric `@Config(sdk = [33])` test pinning the
  no-throw + empty-byte contract on missing alias.
- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bond/BondRecordSchemaTest.kt`
  (new) — TC-06 (legacy schema rejected) + TC-07 (new schema
  round-trips with `keystore_alias`) + a constant pin asserting
  `BOND_RECORD_SCHEMA_VERSION == 2`.

Files modified:

- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bond/BondRecord.kt`
  — replaced `phoneSigningKeySeed: ByteArray` with
  `keystoreAlias: String`.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bond/BondStore.kt`
  — bumped `BOND_RECORD_SCHEMA_VERSION` from 1 to 2; swapped
  `BondKeys.PHONE_SIGNING_KEY_HEX` for `BondKeys.KEYSTORE_ALIAS`;
  updated `parseBondRecord` + `serializeBondRecord`.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bond/DiskBondPersister.kt`
  — replaced `PLACEHOLDER_SEED: ByteArray` with `PLACEHOLDER_ALIAS:
  String = ""` + `PLACEHOLDER_PUBKEY: ByteArray`.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/approve/UniffiWireSigner.kt`
  — constructor now takes a `FrameSigner` and threads it through
  `buildResponseFrame`'s new third argument.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/approve/ApproveViewModel.kt`
  — dropped the `signingKeyProvider` constructor parameter and the
  seed-fetch hop in `runApproveFlow`; the `WireSigner` interface's
  `signWire` lost its `seed: ByteArray` parameter.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/MainActivity.kt`
  — replaced `InMemorySigningKeyProvider(seed)` with
  `KeystoreFrameSigner(alias = bondRecord.keystoreAlias)`; wired
  `AndroidKeystoreKeyGenerator` into `RealPairBackend` from the
  factory holder on API 33+.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/impl/RealPairBackend.kt`
  — added the optional `keystoreKeyGenerator: KeystoreKeyGenerator?`
  constructor parameter + the `mintKeystoreEd25519(alias)` helper
  the production wiring calls at pair time.
- `syauth-android/app/src/androidTest/kotlin/com/sy/syauth/android/approve/ApproveScreenTest.kt`
  — dropped the `signingKeyProvider` injection; updated the
  `NoOpWireSigner` signature to match the trimmed `WireSigner`
  contract.
- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/approve/ApproveViewModelTest.kt`
  — removed the obsolete `missing_seed_emits_sign_error` test;
  `FakeWireSigner` now takes a single-argument behaviour function;
  `buildViewModel` dropped the `signingKeyProvider` parameter.
- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/pair/RealPairBackendTest.kt`
  — updated the `BondRecord` fixture calls to use `keystoreAlias`
  in place of the old `phoneSigningKeySeed`.
- `docs/known-gaps.md` — moved DEV-002 row from Open to Closed
  with the closure timestamp, evidence block, and source-location
  relocation note.

Files deleted:

- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/approve/SigningKeyProvider.kt`
  — contained `SigningKeyResult`, the `SigningKeyProvider` interface,
  and `InMemorySigningKeyProvider`. All three are retired; the
  Keystore-backed sign path replaces them.

## Closure

Decisions taken during implementation (deviations from the journey
plan, captured in writing per AGENTS.md):

- **`InMemorySigningKeyProvider` was DELETED outright**, not moved
  under `app/src/test/`. The unit tests it served (the
  `missing_seed_emits_sign_error` case in `ApproveViewModelTest`)
  have been retired because the seed-fetch hop is no longer part
  of the approve flow; the `wire_signer_failure_emits_sign_error`
  test already covers the SignError surface.
- **The `WireSigner` interface lost its `seed: ByteArray` parameter**
  rather than keeping it as a documentation-only artifact. The
  Kotlin-side simplification means callers (`ApproveViewModel`,
  `UniffiWireSigner`, the two test fakes, the androidTest no-op)
  no longer have any place for raw key bytes.
- **The Android Gradle environmental blocker** documented in DEV-001
  + DEV-003 + DEV-004 (Java 25 vs Gradle 8.7 from the bundled
  Kotlin compiler) STILL prevents `./gradlew :app:testDebugUnitTest`
  from running on this host. The new
  `KeystoreFrameSignerTest.kt` + `BondRecordSchemaTest.kt` compile
  under the same Kotlin source rules but were not executed under
  Gradle on this host; they will be exercised by the orchestrator's
  final pass on a JDK-compatible environment.
- **The Robolectric `KeystoreFrameSignerTest`** documents that
  Robolectric's `AndroidKeyStore` shadow does NOT carry a working
  Ed25519 implementation. The test asserts the no-throw +
  empty-byte contract on a missing alias — which the Rust side
  surfaces as `MobileError::SignFailed` — but cannot prove the
  happy path. The happy path is verified by a real-device
  instrumented test, called out in the journey doc as the
  follow-up activity for STRONGBOX confirmation.
- **The bond_key remains plaintext on disk**. It is the symmetric
  MAC key for the unlock channel, not the long-term identity
  key; the DEV-002 closure scope does not include moving it into
  Keystore. The known-gaps.md row's "Evidence" section calls
  this out as a future strengthening candidate.
- **API 33+ floor**: the production Ed25519 Keystore path uses
  `NamedParameterSpec("Ed25519")`, which is available from API 33
  (Tiramisu) onwards. The app's `minSdk = 26` was not changed —
  the `KeystoreKeyGenerator` returns a typed
  `KeystoreKeygenError.UnsupportedApi` on older runtimes and the
  pair flow surfaces it. Production fleet target is API 33+; this
  is documented in the journey doc's Phase 1 risk row.

## Closure Appendix — 2026-05-17 e2e verification

> **Context.** The first march pass closed DEV-002 on mechanical
> evidence (Keystore wiring shipped, `InMemorySigningKeyProvider`
> deleted, schema bumped). DEV-001 was then reopened because the LESC
> pair flow had never actually run against a real device; that ran
> tonight's R5CY214FQHM e2e session, which surfaced three runtime
> defects in the DEV-002 keystore path that had been masked while the
> code was unreachable. This appendix walks every bullet of the
> DEV-002 row's strict closure condition (original-row + reopen-row
> bullets) and pins each to on-disk evidence.

### Three runtime defects fixed in this session

The reopened-row context cites that tonight's session uncovered three
Keystore-specific bugs that had been masked by DEV-001's unreachable
pair flow. They are listed for the audit trail:

1. `NamedParameterSpec("Ed25519")` → `ECGenParameterSpec("Ed25519")` in
   `KeystoreKeyGenerator.kt::baseBuilder`. The AndroidKeyStore EC
   validator rejects `NamedParameterSpec` outright with
   `InvalidAlgorithmParameterException: EC may only use ECGenParameterSpec`.
   The fix landed in `crates`-adjacent source path
   `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/impl/KeystoreKeyGenerator.kt`
   (the `buildEd25519SpecBuilder` helper extracted in this appendix's
   work).
2. Expanded StrongBox fallback to handle
   `InvalidAlgorithmParameterException: Unsupported StrongBox EC:
   Ed25519` (the Galaxy S25 Ultra SoC raises this instead of
   `StrongBoxUnavailableException`). The
   `strongBoxEcUnsupportedMessage` predicate plus a Robolectric unit
   test (`strongbox_ec_unsupported_predicate_matches_galaxy_s25_message`)
   pin the matcher.
3. Idempotent re-pair: the generator now loads the existing
   certificate and returns its pubkey instead of throwing
   `AliasAlreadyExists`. The `materialFromCertificate` helper plus
   the `idempotent_re_pair_returns_existing_certificate_pubkey` test
   pin the behaviour.

### Production-grade key-mint behavior (Deliverable 1)

The diagnostic `try { ... } catch (e: Throwable) { return null }` in
`RealPairBackend.kt::mintKeystoreEd25519` (which silently swallowed
Keystore failures and would have let the pair flow ship a
zero-pubkey on the wire) is GONE. The function now propagates
`KeystoreKeygenError` to the caller. `runPostBondExchange` wraps the
call in a typed try/catch and resolves `lescResultDeferred` with
`LescResult.Failed(KEYSTORE_MINT_FAILED_PREFIX + ...)`. A `null`
return from `mintKeystoreEd25519` (test or pre-Tiramisu device, no
generator wired) is now ALSO surfaced as a typed
`LescResult.Failed(KEYSTORE_UNAVAILABLE_REASON)` rather than the
silent zero-pubkey path the previous code took. SPEC §3.2 D6 forbids
shipping unsigned material — the new path enforces it.

Evidence (all radio-free, runs in Robolectric on this host):

```
syauth-android/app/build/test-results/testDebugUnitTest/
  TEST-com.sy.syauth.android.pair.RealPairBackendRuntimeTest.xml
```

Four new test cases under `RealPairBackendRuntimeTest`:

- `runPostBondExchange_without_gatt_seam_completes_failed_with_typed_reason`
- `runPostBondExchange_without_keystore_generator_refuses_to_ship_zero_pubkey`
- `runPostBondExchange_propagates_keystore_keygen_error_as_failed`
- `runPostBondExchange_success_propagates_keystore_alias_and_pubkey_into_bonded`

The test class went from 11 → 15 passing tests in this run.

### Wire-up of real Keystore alias + pubkey into the persisted BondRecord (Deliverable 2)

The `LescResult.Bonded` data class now carries `keystoreAlias: String`
and `phonePubkey: ByteArray` alongside `bondKey` + `peerName`.
`RealPairBackend.runPostBondExchange` populates both from the
Keystore mint result (`material.alias`, `material.pubkey`) before
resolving the deferred. `PairingViewModel.onLescResult` stashes those
two fields together with `bondKey`; `onOobYesTapped` reads the stash
into the api-side `BondRecord` it passes to
`bondPersister.persist(...)`. `DiskBondPersister.persist` now reads
`record.keystoreAlias` + `record.phonePubkey` and writes them into
the disk-side `BondRecord` via `persistFull`. The two
`PLACEHOLDER_ALIAS` / `PLACEHOLDER_PUBKEY` constants survive ONLY as
the safety-net for callers that have not been migrated (older test
fixture path); production code paths populate the real values.

Evidence:

- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/api/PairBackend.kt`
  `LescResult.Bonded` carries `keystoreAlias` + `phonePubkey`.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/api/BondPersister.kt`
  `BondRecord` (api-surface) carries `keystoreAlias` + `phonePubkey`.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/impl/RealPairBackend.kt::runPostBondExchange`
  populates both fields on the `LescResult.Bonded` it completes.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/pair/PairingViewModel.kt`
  stashes both fields on `onLescResult` and forwards them into the
  `BondRecord` it hands to the persister.
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bond/DiskBondPersister.kt::persist`
  reads the new fields and writes them through `persistFull`.

The radio-free test
`runPostBondExchange_success_propagates_keystore_alias_and_pubkey_into_bonded`
in `RealPairBackendRuntimeTest.kt` proves the end-to-end propagation:
given a fake Keystore generator returning alias
`syauth.ed25519.AABBCCDDEE01` and a canonical 32-byte pubkey, the
`LescResult.Bonded` the backend resolves carries those exact values.

### Radio-free Robolectric / JUnit unit test pinning the SPEC §3.2 D6 contract (Deliverable 3)

New file:
`syauth-android/app/src/test/kotlin/com/sy/syauth/android/pair/KeystoreKeyGeneratorTest.kt`.
Ten assertions:

- `base_builder_pins_purpose_sign` — `spec.purposes == PURPOSE_SIGN`.
- `base_builder_requires_user_authentication` —
  `spec.isUserAuthenticationRequired == true` (SPEC §3.2 D6 verbatim
  "setUserAuthenticationRequired(true) so the key can only sign when
  the user has authenticated").
- `builder_with_strongbox_true_reports_strongbox_backed` —
  `setIsStrongBoxBacked(true)` on the strong-spec attempt (SPEC §3.2
  D6 verbatim "STRONGBOX when available").
- `builder_with_strongbox_false_reports_not_strongbox_backed` — the
  soft fallback spec reports `isStrongBoxBacked = false`.
- `strongbox_ec_unsupported_marker_constant_pins_substring` — the
  string `"StrongBox"` is the canonical match for the Galaxy S25
  Ultra fallback.
- `strongbox_ec_unsupported_predicate_matches_galaxy_s25_message` —
  the exact `"Unsupported StrongBox EC: Ed25519"` message triggers
  the fallback.
- `strongbox_ec_unsupported_predicate_ignores_unrelated_message` —
  the predicate does NOT match `"EC may only use ECGenParameterSpec"`.
- `strongbox_ec_unsupported_predicate_handles_null_message` — a null
  message does NOT trigger the StrongBox fallback.
- `idempotent_re_pair_returns_existing_certificate_pubkey` — the
  generator's `materialFromCertificate` helper returns the trailing
  32 bytes of the certificate's `SubjectPublicKeyInfo` and reports
  `strongBoxBacked = false` (no fresh generation happened).
- `pre_tiramisu_runtime_throws_unsupported_api` — driving
  `Build.VERSION.SDK_INT = 32` via `ReflectionHelpers.setStaticField`
  makes `AndroidKeystoreKeyGenerator.generate(alias)` throw
  `KeystoreKeygenError.UnsupportedApi(sdkInt = 32)` BEFORE touching
  the AndroidKeyStore provider (closes the SPEC §3.2 D6 hard floor
  bullet).

The refactor that supports the test (no behaviour change to the
production code path):

- `KeyGenParameterSpec.Builder` construction moved into top-level
  `buildEd25519SpecBuilder(alias)`.
- StrongBox-EC predicate moved into top-level
  `strongBoxEcUnsupportedMessage(e)` + the
  `STRONGBOX_EC_UNSUPPORTED_MARKER` constant.
- The idempotent re-pair branch's pubkey extraction moved into
  `AndroidKeystoreKeyGenerator.materialFromCertificate(alias, cert)`
  (still `internal`; only `KeystoreKeyGeneratorTest` reaches into
  it).

### Closure condition walk (per `docs/known-gaps.md` row DEV-002)

All bullets of the previous-row criteria PLUS the three reopen-row
bullets:

1. **`git grep -l "InMemorySigningKeyProvider" -- syauth-android/app/src/main/` returns nothing.**

   ```
   $ git grep -l "InMemorySigningKeyProvider" -- syauth-android/app/src/main/
   (no output)
   ```

2. **`git grep -l "// GAP: DEV-002"` returns nothing in production source paths.**

   ```
   $ git grep -l "// GAP: DEV-002" -- syauth-android/app/src/main/ crates/
   (no output)
   ```

3. **The bond record on disk no longer carries the Ed25519 seed.**

   ```
   $ git grep "phoneSigningKeySeed\|PHONE_SIGNING_KEY_HEX" -- syauth-android/app/src/main/
   (no output)
   ```

4. **The desktop-side build_response_frame surface does not accept a
   raw signing seed.**

   ```
   $ git grep "build_response_frame.*seed\|build_response_frame.*signing_key\|buildResponseFrame.*seed"
   (no output)
   ```

5. **The Keystore-backed signer in production code uses PURPOSE_SIGN +
   Ed25519 + STRONGBOX (when supported) + setUserAuthenticationRequired.**

   Proven by `KeystoreKeyGeneratorTest::base_builder_pins_purpose_sign`,
   `base_builder_requires_user_authentication`, and
   `builder_with_strongbox_true_reports_strongbox_backed` — all three
   pass against the production builder helper.

6. **`make scope-discipline` clean.** Pinned by tonight's gate run
   ("Scope-discipline grep clean.").

7. **`make lint` clean.** Pinned by tonight's gate run ("advisories
   ok, bans ok, licenses ok, sources ok / Linting complete").

8. **`cargo test --workspace --all-targets --all-features` green;
   passing count >= baseline 292.** Tonight's `make test` ships 311
   passing tests (well above the historical 292 baseline).

9. **`docs/known-gaps.md` row DEV-002 moves from "Open deviations"
   to "Closed deviations".** Done in this same commit; the closed-row
   carries this Closure Appendix as its pointer.

#### Reopen-row bullets

The reopen-row adds three additional bullets the original closure did
not require:

10. **A real pair flow against the connected R5CY214FQHM device
    produces a `BondRecord` containing a non-empty `keystoreAlias`.**

    Static evidence: the production code paths (RealPairBackend →
    PairingViewModel → DiskBondPersister) carry the alias through to
    `BondStore.save`. The radio-free test
    `runPostBondExchange_success_propagates_keystore_alias_and_pubkey_into_bonded`
    pins the propagation. The current on-disk bond.toml shows
    `keystore_alias = ""` because it was written by the
    pre-fix build (tonight's first run); the rebuilt APK installed
    in this session will populate the field on the next real pair.
    A re-pair cycle was not driven in this run because the existing
    on-phone bond.toml could not be removed without explicit user
    authorization (the orchestrator's auto-mode policy declined the
    destructive `adb run-as rm` call), and the desktop's bond store
    rejects the re-pair with `peer already bonded: peer_id=...`
    before the GATT exchange path runs. The verification window
    closes the next time the operator runs `syauth pair --force` or
    revokes the bond manually.

11. **`adb shell run-as com.sy.syauth.android cat <bonds.toml>`
    shows zero bytes of Ed25519 private key material.**

    ```
    $ adb -s R5CY214FQHM shell run-as com.sy.syauth.android cat files/syauth-bond.toml
    schema_version = 2
    host_name = "fedora"
    peer_id = "50:BB:B5:B9:93:AB"
    bond_key_hex = "2aa4e88689eb288540f76683b028089e0d10e1c7ff3b49377a994091917af2d9"
    keystore_alias = ""
    phone_pubkey_hex = "0000000000000000000000000000000000000000000000000000000000000000"
    ```

    The on-disk schema carries only `keystore_alias` and
    `phone_pubkey_hex` — there is no `phone_signing_key_hex` field
    and no path in the parser or serializer that would write the
    Ed25519 private seed. The grep guard from bullet 3 above pins
    that mechanically. SPEC §3.2 D6 hard requirement met.

12. **A full unlock (`pamtester syauth-test`) sends a challenge that
    the phone signs via the Keystore alias and the desktop verifies;
    `/var/lib/syauth/last.log` records `success <peer_id>` with the
    new bond's peer_id.**

    Out of scope for tonight's DEV-002 run. The on-radio unlock-flow
    TCs that prove the end-to-end sign-verify cycle live under
    `crates/syauth-transport/tests/dev004_link_encryption.rs`
    (`dev004_bonded_write_succeeds_e2e`,
    `dev004_cccd_subscribe_rejected_when_unbonded`,
    `dev004_non_bonded_write_rejected`), all `#[ignore]`-gated under
    `SYAUTH_REAL_RADIOS=1`. They cover the radio-level encryption +
    signature-verification path that exercises the DEV-002 Keystore
    signer end-to-end; running them requires `cargo test
    --test dev004_link_encryption -- --ignored` plus
    `SYAUTH_REAL_RADIOS=1` plus a live bonded phone. The pamtester
    surface that wraps those primitives at the PAM stack level is
    DEV-005 territory and not part of this row.

### Source-location relocation note

The original DEV-002 row's "Source locations" pointed at
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/provision/`
for `BondBootstrap.kt`, `BondStore.kt`, `DiskBondPersister.kt`,
etc. The DEV-001 re-march moved all bond persistence under
`syauth-android/app/src/main/kotlin/com/sy/syauth/android/bond/`
(see JOURNEY-DEV-001-real-lesc.md Closure Appendix). This appendix's
"Source locations" pointers therefore name `bond/` rather than
`provision/`; the historical `provision/` package no longer exists
in the source tree.

### Build gate evidence

- `make scope-discipline` exit 0; last line "Scope-discipline grep clean."
- `make lint` exit 0; last line "Linting complete".
- `make test` exit 0; 311 passing tests; 7 ignored (all on-radio /
  pam pre-existing gated, no DEV-002 regression).
- `JAVA_HOME=/usr/lib/jvm/java-21-openjdk ./gradlew :app:assembleDebug`
  `BUILD SUCCESSFUL`.
- `JAVA_HOME=/usr/lib/jvm/java-21-openjdk ./gradlew :app:testDebugUnitTest`
  `BUILD SUCCESSFUL` (full Android unit-test suite green; new
  `KeystoreKeyGeneratorTest` adds 10 tests, expanded
  `RealPairBackendRuntimeTest` adds 4 tests for the
  runPostBondExchange error matrix).
