// Roadmap item S-017 — Ed25519 seed provider seam.
//
// The wire-protocol signature is Ed25519 (per SPEC §1, §3, §4.1) and
// produced by the Rust core via UniFFI's `signChallengeResponse(seed,
// frame_bytes)`. The Android Keystore does not yet expose Ed25519 as
// a first-class key algorithm on every Android 13 device (Ed25519
// support landed in API 33 and is not universal). Until that gap
// closes, the Ed25519 seed lives behind this small interface.
//
// In production (S-018 wires the real backing), the seed is loaded
// from a Keystore-encrypted file at app start and held in a
// `ByteArray` that is zeroed when the ViewModel's coroutine scope is
// cancelled. The S-017 PR includes the interface and a tiny
// `InMemorySigningKeyProvider` for tests; the production wiring (read
// seed from file, decrypt via Keystore-backed Cipher) lands alongside
// the background bridge.
//
// The seed never touches the Compose layer directly — only the
// ViewModel ever asks the provider, and the ViewModel hands the bytes
// straight to UniFFI's `signChallengeResponse`. The "crypto code never
// sees the private key bytes" requirement of the S-017 DoD is met at
// the *core* boundary (Rust crypto) but not at the *Kotlin* boundary
// until Keystore Ed25519 lands; this gap is documented in
// `docs/android-setup.md`.
package com.sy.syauth.android.approve

/**
 * Result of a [SigningKeyProvider.loadSeed] call. The seed is the
 * 32-byte Ed25519 secret key bytes; the variant carries a typed
 * failure so the ViewModel can report it without inspecting an
 * exception.
 */
public sealed class SigningKeyResult {
    public data class Ok(val seed: ByteArray) : SigningKeyResult() {
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is Ok) return false
            return seed.contentEquals(other.seed)
        }

        override fun hashCode(): Int = seed.contentHashCode()
    }

    public data class Missing(val reason: String) : SigningKeyResult()
}

/**
 * Contract for fetching the Ed25519 seed used by UniFFI's
 * `signChallengeResponse`. Production loads from a Keystore-encrypted
 * file (lands in S-018); tests use [InMemorySigningKeyProvider].
 */
public interface SigningKeyProvider {
    /**
     * Return the 32-byte Ed25519 seed or a typed `Missing` variant.
     * Implementations MUST NOT throw — every failure becomes a
     * `Missing(reason)`.
     */
    public suspend fun loadSeed(): SigningKeyResult
}

/**
 * Test-only provider that returns a fixed seed. Production code must
 * not use this; it lives in the production module purely so tests in
 * the unit-test source set don't need to declare a duplicate fake.
 *
 * The constructor copies the bytes so the caller can clear their own
 * buffer immediately.
 */
public class InMemorySigningKeyProvider(seed: ByteArray) : SigningKeyProvider {
    private val storedSeed: ByteArray = seed.copyOf()

    override suspend fun loadSeed(): SigningKeyResult = SigningKeyResult.Ok(storedSeed.copyOf())
}
