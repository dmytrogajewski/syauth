// syauth — provision parser unit tests.
//
// Validates the hand-rolled single-line TOML reader against the
// canonical example produced by the desktop's `syauth provision-test`
// subcommand, plus the typed error surface for every documented
// failure mode (missing field, malformed hex, wrong schema_version).
//
// Pure JVM: no Robolectric, no Android side-effects. The parser is a
// pure function from `String -> ProvisionPackage`.
package com.sy.syauth.android.provision

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

private const val FIXTURE_HOST_NAME: String = "fedora"
private const val FIXTURE_PEER_ID: String =
    "0123456789abcdef0123456789abcdef"
private const val FIXTURE_BOND_KEY_HEX: String =
    "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
private const val FIXTURE_SIGNING_KEY_HEX: String =
    "1011121314151617181920212223242526272829303132333435363738394041"
private const val FIXTURE_PUBKEY_HEX: String =
    "4243444546474849505152535455565758596061626364656667686970717273"
private const val FIXTURE_CREATED_AT: String = "2026-05-17T10:00:00Z"

private fun fixture(
    overrides: Map<String, String?> = emptyMap(),
    overrideSchemaVersion: Int? = PROVISION_SCHEMA_VERSION,
): String {
    val lines = mutableListOf<String>()
    if (overrideSchemaVersion != null) {
        lines += "schema_version = $overrideSchemaVersion"
    }
    val defaults = linkedMapOf(
        "host_name" to FIXTURE_HOST_NAME,
        "peer_id" to FIXTURE_PEER_ID,
        "bond_key_hex" to FIXTURE_BOND_KEY_HEX,
        "phone_signing_key_hex" to FIXTURE_SIGNING_KEY_HEX,
        "phone_pubkey_hex" to FIXTURE_PUBKEY_HEX,
        "created_at" to FIXTURE_CREATED_AT,
    )
    for ((key, default) in defaults) {
        val value = if (overrides.containsKey(key)) overrides.getValue(key) else default
        if (value == null) continue
        lines += "$key = \"$value\""
    }
    return lines.joinToString(separator = "\n")
}

class ProvisionPackageParserTest {

    @Test
    fun happy_path_parses_canonical_fixture() {
        val pkg = parseProvisionPackage(fixture())

        assertEquals(PROVISION_SCHEMA_VERSION, pkg.schemaVersion)
        assertEquals(FIXTURE_HOST_NAME, pkg.hostName)
        assertEquals(FIXTURE_PEER_ID, pkg.peerId)
        assertEquals(PROVISION_KEY_BYTES, pkg.bondKey.size)
        assertEquals(PROVISION_KEY_BYTES, pkg.phoneSigningKeySeed.size)
        assertEquals(PROVISION_KEY_BYTES, pkg.phonePubkey.size)
        assertEquals(FIXTURE_CREATED_AT, pkg.createdAt)
        // Spot-check hex decoding: first byte of bond_key is 0x00, second 0x11.
        assertEquals(0x00.toByte(), pkg.bondKey[0])
        assertEquals(0x11.toByte(), pkg.bondKey[1])
    }

    @Test
    fun tolerates_blank_lines_and_comments_and_section_markers() {
        val withFluff = """
            # syauth provision file

            [meta]
            ${fixture()}

            # trailing comment
        """.trimIndent()
        val pkg = parseProvisionPackage(withFluff)
        assertEquals(FIXTURE_HOST_NAME, pkg.hostName)
    }

    @Test
    fun rejects_unsupported_schema_version() {
        val body = fixture(overrideSchemaVersion = 99)
        try {
            parseProvisionPackage(body)
            fail("expected UnsupportedSchemaVersion")
        } catch (e: ProvisionParseError.UnsupportedSchemaVersion) {
            assertEquals(99, e.got)
        }
    }

    @Test
    fun rejects_missing_schema_version() {
        val body = fixture(overrideSchemaVersion = null)
        try {
            parseProvisionPackage(body)
            fail("expected MissingField(schema_version)")
        } catch (e: ProvisionParseError.MissingField) {
            assertTrue(e.message!!.contains("schema_version"))
        }
    }

    @Test
    fun rejects_missing_host_name() {
        val body = fixture(overrides = mapOf("host_name" to null))
        try {
            parseProvisionPackage(body)
            fail("expected MissingField(host_name)")
        } catch (e: ProvisionParseError.MissingField) {
            assertTrue(e.message!!.contains("host_name"))
        }
    }

    @Test
    fun rejects_short_hex_bond_key() {
        val body = fixture(overrides = mapOf("bond_key_hex" to "deadbeef"))
        try {
            parseProvisionPackage(body)
            fail("expected MalformedHex")
        } catch (e: ProvisionParseError.MalformedHex) {
            assertEquals(8, e.length)
        }
    }

    @Test
    fun rejects_non_hex_characters() {
        val poisonedHex = "zz".repeat(PROVISION_KEY_BYTES)
        val body = fixture(overrides = mapOf("bond_key_hex" to poisonedHex))
        try {
            parseProvisionPackage(body)
            fail("expected MalformedHex")
        } catch (e: ProvisionParseError.MalformedHex) {
            assertEquals(PROVISION_HEX32_LEN, e.length)
        }
    }

    @Test
    fun round_trip_serialize_then_parse_preserves_values() {
        val original = parseProvisionPackage(fixture())
        val body = serializeProvisionPackage(original)
        val again = parseProvisionPackage(body)
        assertEquals(original.hostName, again.hostName)
        assertEquals(original.peerId, again.peerId)
        assertArrayEquals(original.bondKey, again.bondKey)
        assertArrayEquals(original.phoneSigningKeySeed, again.phoneSigningKeySeed)
        assertArrayEquals(original.phonePubkey, again.phonePubkey)
        assertEquals(original.createdAt, again.createdAt)
    }
}
