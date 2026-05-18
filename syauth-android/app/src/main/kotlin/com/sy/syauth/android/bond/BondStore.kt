// DEV-002: disk-backed bond record on the phone side.
//
// The on-disk format is a list of single-line `key = "value"` records
// under `context.filesDir`. The DEV-002 schema bump from version 1 to
// 2 swaps the `phone_signing_key_hex` field for `keystore_alias` — the
// Ed25519 private key is now Keystore-resident and never appears as
// bytes anywhere on disk or in the JVM. The bond_key MAC secret stays
// on disk (it's the unlock-channel symmetric key, not the long-term
// identity key); moving it into Keystore is a future strengthening
// candidate called out in the DEV-002 journey doc.
//
// Records persisted by an older DEV-001 build (schema_version = 1)
// fail to parse with [BondParseError.UnsupportedSchemaVersion]; the
// caller surfaces a "re-pair required" UI surface and the operator
// re-runs `syauth pair` to mint a fresh Keystore-backed identity.
//
// Threading: every call is synchronous. The file is < 1 KB and the
// API is invoked once per app cold-start (bootstrap) and once per
// successful pair (the persister hop in [PairingViewModel]); a
// background dispatcher would be ceremony with no payoff.
package com.sy.syauth.android.bond

import java.io.File
import java.io.IOException

/** File name (under `filesDir`) holding the persisted bond record. */
public const val BOND_RECORD_FILE_NAME: String = "syauth-bond.toml"

/** Schema version baked into the on-disk record. DEV-002 bumped 1 -> 2. */
public const val BOND_RECORD_SCHEMA_VERSION: Int = 2

/** Hex string length for a 32-byte field (bond_key, pubkey). */
public const val BOND_HEX32_LEN: Int = 64

/** Byte length of every 32-byte field after hex-decoding. */
public const val BOND_KEY_BYTES_LEN: Int = 32

/** Typed failure surface for the bond-record parser. */
public sealed class BondParseError(message: String) : RuntimeException(message) {
    public class MissingField(field: String) :
        BondParseError("missing required bond field: $field")
    public class UnsupportedSchemaVersion(public val got: Int) :
        BondParseError("unsupported bond schema_version: $got")
    public class MalformedLine(public val line: String) :
        BondParseError("malformed bond line: $line")
    public class MalformedHex(field: String, public val length: Int) :
        BondParseError("malformed hex in $field (length=$length, expected $BOND_HEX32_LEN)")
}

/** Required top-level key names in the bond-record file. */
internal object BondKeys {
    const val SCHEMA_VERSION: String = "schema_version"
    const val HOST_NAME: String = "host_name"
    const val PEER_ID: String = "peer_id"
    const val BOND_KEY_HEX: String = "bond_key_hex"
    const val KEYSTORE_ALIAS: String = "keystore_alias"
    const val PHONE_PUBKEY_HEX: String = "phone_pubkey_hex"
}

/**
 * Disk-backed bond persistence. The constructor accepts the storage
 * directory (production: `context.filesDir`; tests: a per-test
 * `TemporaryFolder` root) so the class is fully exercisable on the
 * JVM without Robolectric.
 */
public class BondStore(private val storageDir: File) {

    /** Path the store reads/writes for this instance. */
    public val storePath: File
        get() = File(storageDir, BOND_RECORD_FILE_NAME)

    /**
     * Persist [record] as the canonical bond record. Atomic via
     * tmpfile + rename so a partial write never leaves a corrupt store.
     */
    @Throws(IOException::class)
    public fun save(record: BondRecord) {
        if (!storageDir.exists() && !storageDir.mkdirs()) {
            throw IOException("could not create bond storage dir: $storageDir")
        }
        val tmp = File(storageDir, "$BOND_RECORD_FILE_NAME$TMP_SUFFIX")
        tmp.writeText(serializeBondRecord(record))
        val target = storePath
        if (target.exists() && !target.delete()) {
            tmp.delete()
            throw IOException("could not replace existing bond record: $target")
        }
        if (!tmp.renameTo(target)) {
            tmp.delete()
            throw IOException("could not rename bond tmpfile into place: $target")
        }
    }

    /**
     * Load the persisted record. Returns `null` if the file does not
     * exist (first-launch state). Throws [BondParseError] on a corrupt
     * file; the caller should surface this and refuse to boot rather
     * than silently dropping a malformed bond.
     */
    @Throws(BondParseError::class)
    public fun load(): BondRecord? {
        val target = storePath
        if (!target.exists()) return null
        return parseBondRecord(target.readText())
    }
}

/** Parse a bond-record file body into a [BondRecord]. */
public fun parseBondRecord(body: String): BondRecord {
    val fields: MutableMap<String, String> = HashMap()
    val integerFields: MutableMap<String, Int> = HashMap()
    for (rawLine in body.lineSequence()) {
        val line = rawLine.trim()
        if (line.isEmpty()) continue
        if (line.startsWith(COMMENT_PREFIX)) continue
        if (line.startsWith(SECTION_PREFIX)) continue
        val eq = line.indexOf(KEY_VALUE_SEPARATOR)
        if (eq <= 0) {
            throw BondParseError.MalformedLine(line)
        }
        val key = line.substring(0, eq).trim()
        val value = line.substring(eq + 1).trim()
        if (key.isEmpty() || value.isEmpty()) {
            throw BondParseError.MalformedLine(line)
        }
        if (value.startsWith(QUOTE) && value.endsWith(QUOTE) && value.length >= MIN_QUOTED_LEN) {
            fields[key] = value.substring(1, value.length - 1)
        } else {
            val parsed = value.toIntOrNull() ?: throw BondParseError.MalformedLine(line)
            integerFields[key] = parsed
        }
    }
    val schemaVersion = integerFields[BondKeys.SCHEMA_VERSION]
        ?: throw BondParseError.MissingField(BondKeys.SCHEMA_VERSION)
    if (schemaVersion != BOND_RECORD_SCHEMA_VERSION) {
        throw BondParseError.UnsupportedSchemaVersion(got = schemaVersion)
    }
    val hostName = fields[BondKeys.HOST_NAME]
        ?: throw BondParseError.MissingField(BondKeys.HOST_NAME)
    val peerId = fields[BondKeys.PEER_ID]
        ?: throw BondParseError.MissingField(BondKeys.PEER_ID)
    val bondKey = decodeHex32(fields, BondKeys.BOND_KEY_HEX)
    val keystoreAlias = fields[BondKeys.KEYSTORE_ALIAS]
        ?: throw BondParseError.MissingField(BondKeys.KEYSTORE_ALIAS)
    val pubkey = decodeHex32(fields, BondKeys.PHONE_PUBKEY_HEX)
    return BondRecord(
        peerId = peerId,
        hostName = hostName,
        bondKey = bondKey,
        keystoreAlias = keystoreAlias,
        phonePubkey = pubkey,
    )
}

/** Serialize a [BondRecord] back to the canonical on-disk body. */
public fun serializeBondRecord(record: BondRecord): String {
    val sb = StringBuilder()
    sb.append(BondKeys.SCHEMA_VERSION).append(SERIALIZE_INT_SEP).append(BOND_RECORD_SCHEMA_VERSION)
        .append(NEWLINE)
    sb.append(BondKeys.HOST_NAME).appendQuoted(record.hostName)
    sb.append(BondKeys.PEER_ID).appendQuoted(record.peerId)
    sb.append(BondKeys.BOND_KEY_HEX).appendQuoted(encodeHex(record.bondKey))
    sb.append(BondKeys.KEYSTORE_ALIAS).appendQuoted(record.keystoreAlias)
    sb.append(BondKeys.PHONE_PUBKEY_HEX).appendQuoted(encodeHex(record.phonePubkey))
    return sb.toString()
}

private fun StringBuilder.appendQuoted(value: String) {
    append(SERIALIZE_STRING_SEP).append(QUOTE).append(value).append(QUOTE).append(NEWLINE)
}

private fun decodeHex32(fields: Map<String, String>, key: String): ByteArray {
    val raw = fields[key] ?: throw BondParseError.MissingField(key)
    if (raw.length != BOND_HEX32_LEN) {
        throw BondParseError.MalformedHex(field = key, length = raw.length)
    }
    val out = ByteArray(BOND_KEY_BYTES_LEN)
    for (i in 0 until BOND_KEY_BYTES_LEN) {
        val hi = hexNibble(raw[HEX_PAIR_STRIDE * i])
        val lo = hexNibble(raw[HEX_PAIR_STRIDE * i + 1])
        if (hi < 0 || lo < 0) {
            throw BondParseError.MalformedHex(field = key, length = raw.length)
        }
        out[i] = ((hi shl HEX_NIBBLE_BITS) or lo).toByte()
    }
    return out
}

private fun hexNibble(c: Char): Int = when (c) {
    in '0'..'9' -> c.code - '0'.code
    in 'a'..'f' -> c.code - 'a'.code + HEX_LETTER_OFFSET
    in 'A'..'F' -> c.code - 'A'.code + HEX_LETTER_OFFSET
    else -> -1
}

private fun encodeHex(bytes: ByteArray): String {
    val sb = StringBuilder(bytes.size * HEX_PAIR_STRIDE)
    for (b in bytes) {
        val v = b.toInt() and HEX_BYTE_MASK
        sb.append(HEX_DIGITS[v ushr HEX_NIBBLE_BITS])
        sb.append(HEX_DIGITS[v and HEX_NIBBLE_MASK])
    }
    return sb.toString()
}

/**
 * Resolve the bond record from on-disk store. Returns `null` if no
 * pair has yet completed on this device (first-launch state).
 * Replaces the former `provision/bootstrapBond` which had a
 * provision-file fallback path; that fallback is retired now that
 * pairing happens via the LESC + app-OOB flow per SPEC §3.2 D5.
 */
public fun loadPersistedBond(storageDir: File): BondRecord? {
    val store = BondStore(storageDir)
    return runCatching { store.load() }.getOrNull()
}

private const val TMP_SUFFIX: String = ".tmp"
private const val COMMENT_PREFIX: String = "#"
private const val SECTION_PREFIX: String = "["
private const val KEY_VALUE_SEPARATOR: Char = '='
private const val QUOTE: Char = '"'
private const val NEWLINE: Char = '\n'
private const val SERIALIZE_INT_SEP: String = " = "
private const val SERIALIZE_STRING_SEP: String = " = "
private const val MIN_QUOTED_LEN: Int = 2
private const val HEX_PAIR_STRIDE: Int = 2
private const val HEX_NIBBLE_BITS: Int = 4
private const val HEX_NIBBLE_MASK: Int = 0x0F
private const val HEX_BYTE_MASK: Int = 0xFF
private const val HEX_LETTER_OFFSET: Int = 10
private const val HEX_DIGITS: String = "0123456789abcdef"
