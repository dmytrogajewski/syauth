// DEV-002 Robolectric test for [KeystoreFrameSigner].
//
// Robolectric's `AndroidKeyStore` provider shadow does NOT carry a
// working Ed25519 implementation: the shadow's `KeyPairGenerator`
// returns hand-rolled RSA/EC pairs and the JCE `Signature.getInstance(
// "Ed25519")` only resolves on real-hardware images. We therefore
// CANNOT prove the happy path here — that requires an instrumented
// test on an API 33+ emulator.
//
// What this test CAN prove (and pins so a regression fails loudly):
//
//   - When the configured alias is missing from the AndroidKeyStore,
//     the signer returns an empty `ByteArray` (the SignFailed signal
//     the Rust side maps onto `MobileError::SignFailed`).
//   - The signer does NOT throw — production callers (the UniFFI
//     callback bridge) cannot afford an unchecked throw because the
//     ABI would surface it as a panic.
//
// Journey: specs/journeys/JOURNEY-DEV-002-keystore-strongbox.md
package com.sy.syauth.android.approve

import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [33])
class KeystoreFrameSignerTest {

    @Test
    fun returns_empty_when_alias_is_absent() {
        val signer = KeystoreFrameSigner(alias = MISSING_ALIAS)
        val result = signer.sign(MESSAGE)
        assertEquals(EMPTY_SIGNATURE_LEN, result.size)
    }

    @Test
    fun does_not_throw_on_missing_alias() {
        val signer = KeystoreFrameSigner(alias = MISSING_ALIAS)
        // The contract: NEVER throw. The Rust side detects the empty
        // ByteArray and surfaces SignFailed; an uncaught exception
        // here would panic across the FFI boundary.
        signer.sign(MESSAGE)
    }

    private companion object {
        const val MISSING_ALIAS: String = "syauth.test.absent-alias"
        val MESSAGE: ByteArray = ByteArray(64) { it.toByte() }
        const val EMPTY_SIGNATURE_LEN: Int = 0
    }
}
