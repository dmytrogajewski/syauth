// Journey: specs/journeys/JOURNEY-S-018-phone-notification-history.md
//
// Pure JVM Robolectric-free test: `ChallengeHistoryDao` takes a
// `File` (production wires `app.filesDir`), so a JUnit `@Rule
// TemporaryFolder` is enough.
package com.sy.syauth.android.history

import com.sy.syauth.android.bg.HISTORY_OUTCOME_GRANTED
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class ChallengeHistoryTest {

    @get:Rule
    val tempFolder: TemporaryFolder = TemporaryFolder()

    @Test
    fun renders_last_fifty() {
        val dao = ChallengeHistoryDao(filesDir = tempFolder.root)
        val total = 60
        for (i in 0 until total) {
            dao.insert(
                ChallengeHistoryRecord(
                    id = "id-$i",
                    peerId = "AA:BB:CC:DD:EE:FF",
                    peerIdShort = "DD:EE:FF",
                    hostname = "alex-desktop",
                    outcome = HISTORY_OUTCOME_GRANTED,
                    timestampMs = (1_700_000_000_000L + i).toLong(),
                ),
            )
        }
        val recent = dao.recent(HISTORY_DISPLAY_LIMIT)
        assertEquals(HISTORY_DISPLAY_LIMIT, recent.size)
        // Descending by timestamp_ms.
        val highest = 1_700_000_000_000L + (total - 1)
        assertEquals(highest, recent.first().timestampMs)
        // 50th highest = highest - 49.
        assertEquals(highest - (HISTORY_DISPLAY_LIMIT - 1), recent.last().timestampMs)
        // Order strictly descending.
        for (i in 1 until recent.size) {
            assertTrue(
                "descending invariant: ${recent[i - 1].timestampMs} > ${recent[i].timestampMs}",
                recent[i - 1].timestampMs > recent[i].timestampMs,
            )
        }
    }
}
