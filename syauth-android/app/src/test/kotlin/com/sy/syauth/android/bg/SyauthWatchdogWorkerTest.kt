// Roadmap item S-012 — Robolectric JVM test for the
// `SyauthWatchdogWorker`. Pins the DoD bullet verbatim:
//
//   - `resurrects_killed_service` — given `isRunning = false` and a
//     bond on disk, running the worker queues a `startForegroundService`
//     against `SyauthCompanionService` and returns `Result.success()`.
//
// Journey: specs/journeys/JOURNEY-S-012-boot-receiver-watchdog.md
package com.sy.syauth.android.bg

import android.app.Application
import androidx.test.core.app.ApplicationProvider
import androidx.work.ListenableWorker
import androidx.work.testing.TestListenableWorkerBuilder
import com.sy.syauth.android.bond.BondRecord
import com.sy.syauth.android.bond.BondStore
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

private const val FIXTURE_BOND_KEY_LEN: Int = 32
private const val FIXTURE_HOST: String = "alex-desktop"
private const val FIXTURE_PEER: String = "DD:EE:FF:00:11:22"
private const val FIXTURE_KEYSTORE_ALIAS: String = "syauth.watchdog.alias"

private fun fixtureBond(): BondRecord = BondRecord(
    peerId = FIXTURE_PEER,
    hostName = FIXTURE_HOST,
    bondKey = ByteArray(FIXTURE_BOND_KEY_LEN) { it.toByte() },
    keystoreAlias = FIXTURE_KEYSTORE_ALIAS,
    phonePubkey = ByteArray(FIXTURE_BOND_KEY_LEN) { it.toByte() },
)

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class SyauthWatchdogWorkerTest {

    private val app: Application
        get() = ApplicationProvider.getApplicationContext()

    @After
    fun cleanup() {
        BondStore(app.filesDir).storePath.delete()
        SyauthCompanionService.isRunning.set(false)
    }

    @Test
    fun resurrects_killed_service() {
        SyauthCompanionService.isRunning.set(false)
        BondStore(app.filesDir).save(fixtureBond())

        val worker = TestListenableWorkerBuilder
            .from(app, SyauthWatchdogWorker::class.java)
            .build()
        val result = worker.startWork().get()

        assertEquals(ListenableWorker.Result.success(), result)
        val started = shadowOf(app).nextStartedService
        assertNotNull("expected SyauthCompanionService start, got null", started)
        assertEquals(
            SyauthCompanionService::class.java.name,
            started!!.component?.className,
        )
    }
}
