// syauth — BondStore round-trip tests.
//
// Pure JVM tests against a `TemporaryFolder` — the store accepts any
// directory the caller supplies so production (`context.filesDir`)
// and tests differ only in the path.
package com.sy.syauth.android.provision

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

private const val TEST_HOST_NAME: String = "fedora"
private const val TEST_PEER_ID: String =
    "abcdef0123456789abcdef0123456789"
private const val TEST_CREATED_AT: String = "2026-05-17T10:00:00Z"

private fun samplePackage(): ProvisionPackage = ProvisionPackage(
    schemaVersion = PROVISION_SCHEMA_VERSION,
    hostName = TEST_HOST_NAME,
    peerId = TEST_PEER_ID,
    bondKey = ByteArray(PROVISION_KEY_BYTES) { it.toByte() },
    phoneSigningKeySeed = ByteArray(PROVISION_KEY_BYTES) { (it + 1).toByte() },
    phonePubkey = ByteArray(PROVISION_KEY_BYTES) { (it + 2).toByte() },
    createdAt = TEST_CREATED_AT,
)

class BondStoreTest {

    @get:Rule
    val tmp: TemporaryFolder = TemporaryFolder()

    @Test
    fun load_returns_null_when_file_absent() {
        val store = BondStore(tmp.root)
        assertNull(store.load())
    }

    @Test
    fun save_then_load_round_trips_every_field() {
        val store = BondStore(tmp.root)
        val pkg = samplePackage()

        store.save(pkg)
        val loaded = store.load()

        assertNotNull(loaded)
        assertEquals(pkg.peerId, loaded!!.peerId)
        assertEquals(pkg.hostName, loaded.hostName)
        assertArrayEquals(pkg.bondKey, loaded.bondKey)
        assertArrayEquals(pkg.phoneSigningKeySeed, loaded.phoneSigningKeySeed)
        assertArrayEquals(pkg.phonePubkey, loaded.phonePubkey)
    }

    @Test
    fun save_replaces_existing_record_atomically() {
        val store = BondStore(tmp.root)
        store.save(samplePackage())

        val replacement = samplePackage().copy(
            hostName = "second-host",
        )
        store.save(replacement)

        val loaded = store.load()
        assertEquals("second-host", loaded!!.hostName)
        // The tmp file must be cleaned up so a stat does not see it.
        val children = tmp.root.list().orEmpty()
        assertTrue(
            "expected only one file under storage dir, got ${children.toList()}",
            children.size == 1 && children[0] == BOND_RECORD_FILE_NAME,
        )
    }

    @Test
    fun save_creates_missing_storage_dir() {
        val nested = java.io.File(tmp.root, "subdir/inside")
        val store = BondStore(nested)
        store.save(samplePackage())
        assertTrue(java.io.File(nested, BOND_RECORD_FILE_NAME).exists())
    }
}
