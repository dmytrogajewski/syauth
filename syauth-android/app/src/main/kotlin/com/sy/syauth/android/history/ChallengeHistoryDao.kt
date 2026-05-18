// Roadmap item S-018 — phone-side challenge history audit trail.
//
// Substitutes a JSONL file for the SPEC's "small Room table" wording
// to avoid KSP / room-compiler gradle-plugin churn (the task spec
// flagged this as the pragmatic path; SPEC §3 scope item #23's
// load-bearing claim is the 50-row history surface, NOT the
// underlying storage primitive). Journey:
// specs/journeys/JOURNEY-S-018-phone-notification-history.md.
//
// Storage layout: `${filesDir}/challenge_history.jsonl`, one
// `ChallengeHistoryRecord` per line, append-only with truncate-to-
// `MAX_HISTORY_FILE_RECORDS` on every insert. The display surface
// reads the most-recent `HISTORY_DISPLAY_LIMIT` rows; the extra
// headroom bounds disk growth.
package com.sy.syauth.android.history

import java.io.File

/**
 * File-stem of the on-disk JSONL store. Pinned constant per
 * AGENTS.md "no magic literals" rule and named with the same
 * vocabulary the task spec used (`HISTORY_TABLE_NAME`) so a future
 * Room migration can reuse the name as the table id.
 */
public const val HISTORY_TABLE_NAME: String = "challenge_history"

/**
 * Filename extension. JSONL = newline-delimited JSON.
 */
public const val HISTORY_FILE_EXTENSION: String = ".jsonl"

/**
 * Number of rows the HISTORY route renders. SPEC §3 scope item #23:
 * "last 50 transactions".
 */
public const val HISTORY_DISPLAY_LIMIT: Int = 50

/**
 * Hard upper bound on records persisted to disk. We display
 * [HISTORY_DISPLAY_LIMIT] (=50) and keep an extra 150-row buffer so
 * a rare burst of sudos does not erase rows the user wanted to see
 * one screen later. ~30 KiB steady state at ~150 bytes per row.
 */
public const val MAX_HISTORY_FILE_RECORDS: Int = 200

/**
 * Field separator inside each line. We hand-roll a simple
 * tab-separated record format instead of dragging in a JSON
 * library because (a) `org.json.JSONObject` is Android-stubbed at
 * pure-JVM unit-test time and (b) the record is six fixed string
 * fields with no nesting — a TSV-like line is enough.
 */
internal const val HISTORY_FIELD_SEPARATOR: String = "\t"

/** Number of fields in the on-disk line layout. */
internal const val HISTORY_FIELD_COUNT: Int = 6

/**
 * One challenge transaction recorded for the audit history surface.
 *
 * Field shape mirrors the desktop daemon's `/var/lib/syauth/last.log`
 * line layout (SPEC §3 scope item #8) so a future correlation tool
 * can join the two by `id` or `peer_id + timestamp_ms`.
 */
public data class ChallengeHistoryRecord(
    /** Stable per-record UUID v4 (minted at insert time). */
    val id: String,
    /** Full bond mac, e.g. `AA:BB:CC:DD:EE:FF`. */
    val peerId: String,
    /** Last three octets of [peerId], e.g. `DD:EE:FF`. */
    val peerIdShort: String,
    /** Bond's `hostName`. */
    val hostname: String,
    /** One of `granted` / `denied` / `timed-out`. */
    val outcome: String,
    /** Epoch milliseconds at record creation. */
    val timestampMs: Long,
)

/**
 * Append-only audit DAO for challenge transactions.
 *
 * Production: constructed once at first dispatch from the
 * application's `filesDir`. Tests: constructed against a
 * `TemporaryFolder.root` so every case starts from an empty store.
 */
public class ChallengeHistoryDao(
    private val filesDir: File,
) {
    private val storeFile: File
        get() = File(filesDir, HISTORY_TABLE_NAME + HISTORY_FILE_EXTENSION)

    /**
     * Append [record] to the on-disk store, then truncate the store
     * back to the most-recent [MAX_HISTORY_FILE_RECORDS] entries.
     */
    @Synchronized
    public fun insert(record: ChallengeHistoryRecord) {
        val parent = filesDir
        if (!parent.exists()) parent.mkdirs()
        storeFile.appendText(encode(record) + "\n")
        truncateIfNeeded()
    }

    /**
     * Return up to [limit] most-recent records (descending by
     * [ChallengeHistoryRecord.timestampMs]). Malformed lines are
     * silently skipped — a partial line from a crashed write does
     * not crash the read path.
     */
    @Synchronized
    public fun recent(limit: Int): List<ChallengeHistoryRecord> {
        val f = storeFile
        if (!f.exists()) return emptyList()
        val parsed: List<ChallengeHistoryRecord> = f.readLines()
            .mapNotNull { runCatching { decode(it) }.getOrNull() }
        return parsed.sortedByDescending { it.timestampMs }.take(limit)
    }

    private fun truncateIfNeeded() {
        val f = storeFile
        val lines = runCatching { f.readLines() }.getOrDefault(emptyList())
        if (lines.size <= MAX_HISTORY_FILE_RECORDS) return
        val kept = lines.takeLast(MAX_HISTORY_FILE_RECORDS)
        f.writeText(kept.joinToString(separator = "\n", postfix = "\n"))
    }
}

internal fun encode(record: ChallengeHistoryRecord): String =
    listOf(
        record.id,
        record.peerId,
        record.peerIdShort,
        record.hostname,
        record.outcome,
        record.timestampMs.toString(),
    ).joinToString(separator = HISTORY_FIELD_SEPARATOR)

internal fun decode(line: String): ChallengeHistoryRecord {
    val parts = line.split(HISTORY_FIELD_SEPARATOR)
    require(parts.size == HISTORY_FIELD_COUNT) { "expected $HISTORY_FIELD_COUNT fields, got ${parts.size}" }
    return ChallengeHistoryRecord(
        id = parts[0],
        peerId = parts[1],
        peerIdShort = parts[2],
        hostname = parts[3],
        outcome = parts[4],
        timestampMs = parts[5].toLong(),
    )
}
