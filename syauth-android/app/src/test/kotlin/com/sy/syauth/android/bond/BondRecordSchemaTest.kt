// DEV-002 closure probes for the BondRecord on-disk schema bump.
//
// Two TCs:
//
//   TC-06: an on-disk record from a DEV-001 build (schema_version = 1,
//          carrying `phone_signing_key_hex`) MUST be rejected by the
//          DEV-002 parser with [BondParseError.UnsupportedSchemaVersion].
//          The home route surfaces a "re-pair required" prompt on
//          this signal.
//
//   TC-07: a fresh DEV-002 record (schema_version = 2, carrying
//          `keystore_alias`) MUST round-trip: serialize -> parse
//          produces a byte-identical [BondRecord]; the on-disk body
//          carries `keystore_alias` and does NOT carry
//          `phone_signing_key_hex`.
//
// Journey: specs/journeys/JOURNEY-DEV-002-keystore-strongbox.md
package com.sy.syauth.android.bond

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class BondRecordSchemaTest {

    @Test
    fun tc06_legacy_schema_version_1_is_rejected() {
        val legacyBody = buildString {
            appendLine("schema_version = 1")
            appendLine("host_name = \"$TEST_HOSTNAME\"")
            appendLine("peer_id = \"$TEST_PEER_ID\"")
            appendLine("bond_key_hex = \"$TEST_HEX32\"")
            appendLine("phone_signing_key_hex = \"$TEST_HEX32\"")
            appendLine("phone_pubkey_hex = \"$TEST_HEX32\"")
        }
        val thrown = try {
            parseBondRecord(legacyBody)
            null
        } catch (e: BondParseError) {
            e
        }
        assertTrue(
            "expected UnsupportedSchemaVersion, got $thrown",
            thrown is BondParseError.UnsupportedSchemaVersion,
        )
        val err = thrown as BondParseError.UnsupportedSchemaVersion
        assertEquals(LEGACY_SCHEMA_VERSION, err.got)
    }

    @Test
    fun tc07_new_schema_round_trips_with_keystore_alias() {
        val record = BondRecord(
            peerId = TEST_PEER_ID,
            hostName = TEST_HOSTNAME,
            bondKey = ByteArray(BOND_KEY_BYTES_LEN) { it.toByte() },
            keystoreAlias = TEST_KEYSTORE_ALIAS,
            phonePubkey = ByteArray(BOND_KEY_BYTES_LEN) { (it + 1).toByte() },
        )
        val serialized = serializeBondRecord(record)
        assertTrue(
            "serialized body must carry keystore_alias",
            serialized.contains("keystore_alias = \"$TEST_KEYSTORE_ALIAS\""),
        )
        assertFalse(
            "serialized body must NOT carry phone_signing_key_hex",
            serialized.contains("phone_signing_key_hex"),
        )
        val parsed = parseBondRecord(serialized)
        assertEquals(record, parsed)
    }

    @Test
    fun tc07b_schema_version_constant_is_two() {
        assertEquals(NEW_SCHEMA_VERSION, BOND_RECORD_SCHEMA_VERSION)
    }

    private companion object {
        const val TEST_PEER_ID: String = "abcdef0123456789abcdef0123456789"
        const val TEST_HOSTNAME: String = "test-desktop"
        const val TEST_HEX32: String =
            "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20"
        const val TEST_KEYSTORE_ALIAS: String = "syauth.ed25519.test-peer"
        const val LEGACY_SCHEMA_VERSION: Int = 1
        const val NEW_SCHEMA_VERSION: Int = 2
    }
}
