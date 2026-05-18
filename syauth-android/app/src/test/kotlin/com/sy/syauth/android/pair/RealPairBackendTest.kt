// DEV-001: unit tests for the Android pairing-variant gate
// ([PairingBroadcastReceiver]) and the [RealPairBackend] surface that
// replaces the former `StubPairBackend`.
//
// Test matrix (mapped onto JOURNEY-DEV-001-real-lesc.md):
//
//   TC-03 — Just Works variant rejected at the broadcast receiver.
//   TC-04 — App-OOB mismatch surfaces via a different bond_key when
//           the phone's pubkey is substituted (covered on the Rust
//           side; this file pins the Kotlin-visible decision).
//   TC-10 — Persistence failure path on the phone-side BondStore.
//
// Robolectric pins the framework version to API 34 so
// `Intent.getIntExtra` / `BluetoothDevice` constants are present.
package com.sy.syauth.android.pair

import android.bluetooth.BluetoothDevice
import android.content.Intent
import com.sy.syauth.android.bond.BondRecord
import com.sy.syauth.android.bond.BondStore
import com.sy.syauth.android.bond.DiskBondPersister
import com.sy.syauth.android.pair.api.BondRecord as PairBondRecord
import com.sy.syauth.android.pair.api.PersistError
import com.sy.syauth.android.pair.impl.EXTRA_PAIRING_KEY_NAME
import com.sy.syauth.android.pair.impl.EXTRA_PAIRING_VARIANT_NAME
import com.sy.syauth.android.pair.impl.PAIRING_VARIANT_PASSKEY_CONFIRMATION
import com.sy.syauth.android.pair.impl.PairingBroadcastReceiver
import com.sy.syauth.android.pair.impl.PairingVariantDecision
import com.sy.syauth.android.pair.impl.decideAndroidPairingVariant
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

/** Numeric value Android uses for Just Works (variant 3). */
private const val JUST_WORKS_VARIANT: Int = 3

/** Numeric value Android uses for legacy PIN entry (variant 0). */
private const val LEGACY_PIN_VARIANT: Int = 0

/** Pinned 6-digit passkey carried with the LESC numeric-comparison broadcast. */
private const val TEST_PASSKEY: Int = 123_456

/** Pinned bond fixture. */
private const val TEST_PEER_ID: String = "abcdef0123456789abcdef0123456789"
private const val TEST_PEER_NAME: String = "test-desktop"
private const val TEST_ALIAS_A: String = "syauth.ed25519.test-alias-a"
private const val TEST_ALIAS_B: String = "syauth.ed25519.test-alias-b"

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class RealPairBackendTest {

    // -------------------------------------------------------------
    // TC-03 — Just Works variant rejected.
    // -------------------------------------------------------------

    @Test
    fun tc03_decide_android_pairing_variant_accepts_passkey_confirmation() {
        val decision = decideAndroidPairingVariant(PAIRING_VARIANT_PASSKEY_CONFIRMATION, TEST_PASSKEY)
        assertTrue("expected Accept, got $decision", decision is PairingVariantDecision.AcceptPasskeyConfirmation)
        val accept = decision as PairingVariantDecision.AcceptPasskeyConfirmation
        assertEquals(TEST_PASSKEY, accept.passkey)
    }

    @Test
    fun tc03_decide_android_pairing_variant_rejects_just_works() {
        val decision = decideAndroidPairingVariant(JUST_WORKS_VARIANT, TEST_PASSKEY)
        assertTrue("Just Works must reject, got $decision", decision is PairingVariantDecision.Reject)
        val reject = decision as PairingVariantDecision.Reject
        assertEquals(JUST_WORKS_VARIANT, reject.variant)
    }

    @Test
    fun tc03_decide_android_pairing_variant_rejects_legacy_pin() {
        val decision = decideAndroidPairingVariant(LEGACY_PIN_VARIANT, TEST_PASSKEY)
        assertTrue("LegacyPin must reject, got $decision", decision is PairingVariantDecision.Reject)
    }

    @Test
    fun tc03_pairing_broadcast_receiver_invokes_onaccept_for_lesc_variant() {
        var acceptedPasskey: Int? = null
        var rejectCount: Int = 0
        val receiver = PairingBroadcastReceiver(
            onAccept = { acceptedPasskey = it },
            onReject = { rejectCount += 1 },
        )
        val intent = Intent(BluetoothDevice.ACTION_PAIRING_REQUEST).apply {
            putExtra(EXTRA_PAIRING_VARIANT_NAME, PAIRING_VARIANT_PASSKEY_CONFIRMATION)
            putExtra(EXTRA_PAIRING_KEY_NAME, TEST_PASSKEY)
        }
        receiver.onReceive(RuntimeEnvironment.getApplication(), intent)
        assertEquals(TEST_PASSKEY, acceptedPasskey)
        assertEquals(0, rejectCount)
    }

    @Test
    fun tc03_pairing_broadcast_receiver_invokes_onreject_for_just_works() {
        var acceptedPasskey: Int? = null
        var rejectVariant: Int? = null
        val receiver = PairingBroadcastReceiver(
            onAccept = { acceptedPasskey = it },
            onReject = { rejectVariant = it },
        )
        val intent = Intent(BluetoothDevice.ACTION_PAIRING_REQUEST).apply {
            putExtra(EXTRA_PAIRING_VARIANT_NAME, JUST_WORKS_VARIANT)
            putExtra(EXTRA_PAIRING_KEY_NAME, TEST_PASSKEY)
        }
        receiver.onReceive(RuntimeEnvironment.getApplication(), intent)
        assertEquals("onAccept must not be called", null, acceptedPasskey)
        assertEquals(JUST_WORKS_VARIANT, rejectVariant)
    }

    // -------------------------------------------------------------
    // TC-04 — App-OOB mismatch is observable by the persister
    //          (covered cryptographically on the Rust side; this
    //          file pins the Kotlin contract that an attacker who
    //          substitutes a different bond_key produces a different
    //          persisted record).
    // -------------------------------------------------------------

    @Test
    fun tc04_disk_persister_writes_distinct_records_for_distinct_bond_keys() {
        val tmp = java.io.File.createTempFile("syauth-tc04-", "").also {
            it.delete()
            it.mkdirs()
        }
        val store = BondStore(tmp)
        val persister = DiskBondPersister(store)
        persister.persistFull(
            BondRecord(
                peerId = TEST_PEER_ID,
                hostName = TEST_PEER_NAME,
                bondKey = ByteArray(BOND_KEY_BYTES) { it.toByte() },
                keystoreAlias = TEST_ALIAS_A,
                phonePubkey = ByteArray(BOND_KEY_BYTES),
            ),
        )
        val first = store.load()
        assertNotNull(first)
        persister.persistFull(
            BondRecord(
                peerId = TEST_PEER_ID,
                hostName = TEST_PEER_NAME,
                bondKey = ByteArray(BOND_KEY_BYTES) { (it + 1).toByte() },
                keystoreAlias = TEST_ALIAS_B,
                phonePubkey = ByteArray(BOND_KEY_BYTES),
            ),
        )
        val second = store.load()
        assertNotNull(second)
        // Two bond_keys → two distinct on-disk records.
        assertTrue(!first!!.bondKey.contentEquals(second!!.bondKey))
    }

    // -------------------------------------------------------------
    // TC-10 — Persistence failure on phone-side BondStore.
    // -------------------------------------------------------------

    @Test(expected = PersistError::class)
    fun tc10_disk_persister_surfaces_typed_persist_error_when_dir_unwritable() {
        // Point the persister at a path that cannot be created (a file
        // already exists where the dir is expected). The save call
        // surfaces IOException, which the persister wraps as
        // PersistError per the API contract.
        val parent = java.io.File.createTempFile("syauth-tc10-parent-", "")
        // Leave `parent` as a regular FILE — BondStore tries to mkdirs()
        // on a path inside it, which must fail.
        val storageDir = java.io.File(parent, "child-dir-cannot-create")
        val store = BondStore(storageDir)
        val persister = DiskBondPersister(store)
        persister.persist(
            PairBondRecord(
                peerId = TEST_PEER_ID,
                peerName = TEST_PEER_NAME,
                bondKey = ByteArray(BOND_KEY_BYTES) { it.toByte() },
            ),
        )
    }

    // -------------------------------------------------------------
    // DEV-001 re-march — receiver pins the AOSP variant constant
    // value (2). The previous code drift to 4 would have rejected
    // every legit LESC numeric-comparison broadcast.
    // -------------------------------------------------------------

    @Test
    fun dev001_remarch_pairing_variant_passkey_confirmation_constant_equals_two() {
        // AOSP `BluetoothDevice.java` pins the value at 2. Any drift
        // would silently break the gate.
        assertEquals(2, PAIRING_VARIANT_PASSKEY_CONFIRMATION)
    }

    @Test
    fun dev001_remarch_decide_android_pairing_variant_accepts_value_two() {
        val decision = decideAndroidPairingVariant(2, TEST_PASSKEY)
        assertTrue("variant 2 must accept, got $decision", decision is PairingVariantDecision.AcceptPasskeyConfirmation)
    }

    @Test
    fun dev001_remarch_decide_android_pairing_variant_rejects_value_four() {
        // Variant 4 is PAIRING_VARIANT_DISPLAY_PASSKEY per AOSP —
        // distinct from LESC numeric comparison and must be refused.
        val decision = decideAndroidPairingVariant(4, TEST_PASSKEY)
        assertTrue("variant 4 must reject, got $decision", decision is PairingVariantDecision.Reject)
    }

    private companion object {
        /** 32 bytes — the LESC-derived `bond_key` width. */
        const val BOND_KEY_BYTES: Int = 32
    }
}
