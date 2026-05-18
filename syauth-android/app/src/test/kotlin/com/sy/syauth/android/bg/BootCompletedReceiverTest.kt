// Roadmap item S-012 — Robolectric JVM tests for the
// `BootCompletedReceiver`. Pins the DoD bullets verbatim:
//
//   1. `boot_with_bond_starts_service` — pre-seeds a `BondRecord` on
//      disk in the application's `filesDir` and asserts that
//      delivering an `Intent.ACTION_BOOT_COMPLETED` broadcast queues a
//      `startForegroundService` targeting `SyauthCompanionService`.
//   2. `boot_without_bond_no_op` — no bond on disk → no service was
//      started.
//
// Journey: specs/journeys/JOURNEY-S-012-boot-receiver-watchdog.md
package com.sy.syauth.android.bg

import android.app.Application
import android.content.Intent
import androidx.test.core.app.ApplicationProvider
import com.sy.syauth.android.bond.BondRecord
import com.sy.syauth.android.bond.BondStore
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

private const val FIXTURE_BOND_KEY_LEN: Int = 32
private const val FIXTURE_HOST: String = "alex-desktop"
private const val FIXTURE_PEER: String = "AA:BB:CC:DD:EE:FF"
private const val FIXTURE_KEYSTORE_ALIAS: String = "syauth.test.alias"

private fun fixtureBond(): BondRecord = BondRecord(
    peerId = FIXTURE_PEER,
    hostName = FIXTURE_HOST,
    bondKey = ByteArray(FIXTURE_BOND_KEY_LEN) { it.toByte() },
    keystoreAlias = FIXTURE_KEYSTORE_ALIAS,
    phonePubkey = ByteArray(FIXTURE_BOND_KEY_LEN) { it.toByte() },
)

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class BootCompletedReceiverTest {

    private val app: Application
        get() = ApplicationProvider.getApplicationContext()

    @After
    fun cleanup() {
        // Remove any bond fixture so cases don't leak into each other.
        BondStore(app.filesDir).storePath.delete()
    }

    @Test
    fun boot_with_bond_starts_service() {
        BondStore(app.filesDir).save(fixtureBond())

        BootCompletedReceiver().onReceive(app, Intent(BOOT_COMPLETED_ACTION))

        val started = shadowOf(app).nextStartedService
        assertNotNull("expected SyauthCompanionService start, got null", started)
        assertEquals(
            SyauthCompanionService::class.java.name,
            started!!.component?.className,
        )
    }

    @Test
    fun boot_without_bond_no_op() {
        // No bond on disk.
        BootCompletedReceiver().onReceive(app, Intent(BOOT_COMPLETED_ACTION))

        val started = shadowOf(app).nextStartedService
        assertNull("expected no service start, got $started", started)
    }
}
