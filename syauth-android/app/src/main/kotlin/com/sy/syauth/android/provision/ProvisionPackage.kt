// syauth — provision package data and parser.
//
// The desktop's `syauth provision-test` subcommand emits a TOML file
// whose structure is documented in
// `crates/syauth-cli/src/provision.rs`. The phone consumes that file
// on first launch to bootstrap its bond state (shared `bond_key` for
// MAC verification, plus an Ed25519 seed/public key the phone uses to
// sign challenge responses).
//
// This module owns:
//
//   * [ProvisionPackage] — in-memory representation of the TOML file.
//   * A hand-rolled single-line `key = "value"` parser. The file format
//     is intentionally minimal (one key per line; quoted strings; an
//     integer schema_version) so a 30-line parser suffices and the app
//     avoids pulling in a real TOML dependency for a single read site.
//   * [ProvisionParseError] — typed error surface for the parser.
//
// The parser is strict: every required field must be present, lengths
// of the hex-encoded fields must match the wire contract, and the
// schema_version must equal [PROVISION_SCHEMA_VERSION]. Any deviation
// throws — the caller surfaces this as a user-visible toast and refuses
// to bootstrap.
package com.sy.syauth.android.provision

/**
 * Schema version baked into the provision file by the desktop CLI.
 * Mirrors `PROVISION_SCHEMA_VERSION` in `crates/syauth-cli/src/provision.rs`.
 */
public const val PROVISION_SCHEMA_VERSION: Int = 1

/** Hex string length for a 32-byte field (bond_key, seed, pubkey). */
public const val PROVISION_HEX32_LEN: Int = 64

/** Byte length of every 32-byte field after hex-decoding. */
public const val PROVISION_KEY_BYTES: Int = 32

/** Required top-level key names in the provision package. */
internal object ProvisionKeys {
    const val SCHEMA_VERSION: String = "schema_version"
    const val HOST_NAME: String = "host_name"
    const val PEER_ID: String = "peer_id"
    const val BOND_KEY_HEX: String = "bond_key_hex"
    const val PHONE_SIGNING_KEY_HEX: String = "phone_signing_key_hex"
    const val PHONE_PUBKEY_HEX: String = "phone_pubkey_hex"
    const val CREATED_AT: String = "created_at"
}

/**
 * Parsed view of `syauth-provision.toml`. The three `*Hex` fields in
 * the source TOML become `ByteArray` here (hex-decoded eagerly so a
 * malformed file is rejected at parse time, not at first use).
 */
public data class ProvisionPackage(
    val schemaVersion: Int,
    val hostName: String,
    val peerId: String,
    val bondKey: ByteArray,
    val phoneSigningKeySeed: ByteArray,
    val phonePubkey: ByteArray,
    val createdAt: String,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is ProvisionPackage) return false
        return schemaVersion == other.schemaVersion &&
            hostName == other.hostName &&
            peerId == other.peerId &&
            bondKey.contentEquals(other.bondKey) &&
            phoneSigningKeySeed.contentEquals(other.phoneSigningKeySeed) &&
            phonePubkey.contentEquals(other.phonePubkey) &&
            createdAt == other.createdAt
    }

    override fun hashCode(): Int {
        var result = schemaVersion
        result = 31 * result + hostName.hashCode()
        result = 31 * result + peerId.hashCode()
        result = 31 * result + bondKey.contentHashCode()
        result = 31 * result + phoneSigningKeySeed.contentHashCode()
        result = 31 * result + phonePubkey.contentHashCode()
        result = 31 * result + createdAt.hashCode()
        return result
    }
}

/** Typed failure surface for the parser. */
public sealed class ProvisionParseError(message: String) : RuntimeException(message) {
    public class MissingField(field: String) :
        ProvisionParseError("missing required provision field: $field")

    public class UnsupportedSchemaVersion(public val got: Int) : ProvisionParseError(
        "unsupported provision schema_version: got $got, expected $PROVISION_SCHEMA_VERSION",
    )

    public class MalformedLine(public val line: String) :
        ProvisionParseError("malformed provision line: $line")

    public class MalformedHex(field: String, public val length: Int) : ProvisionParseError(
        "malformed hex in $field (length=$length, expected $PROVISION_HEX32_LEN)",
    )
}

/**
 * Parser entry point. Accepts the raw TOML body as a string and
 * returns a fully validated [ProvisionPackage] or throws a typed
 * [ProvisionParseError].
 *
 * Grammar (single-line records, ignoring blank lines, comments starting
 * with `#`, and TOML `[section]` markers — the desktop emits a flat
 * top-level table plus one inline documentation key, both are tolerated):
 *
 *     key = "string"
 *     key = integer
 */
public fun parseProvisionPackage(body: String): ProvisionPackage {
    val fields: MutableMap<String, String> = HashMap()
    val integerFields: MutableMap<String, Int> = HashMap()
    for (rawLine in body.lineSequence()) {
        val line = rawLine.trim()
        if (line.isEmpty()) continue
        if (line.startsWith(COMMENT_PREFIX)) continue
        if (line.startsWith(SECTION_PREFIX)) continue
        val eq = line.indexOf(KEY_VALUE_SEPARATOR)
        if (eq <= 0) {
            throw ProvisionParseError.MalformedLine(line)
        }
        val key = line.substring(0, eq).trim()
        val value = line.substring(eq + 1).trim()
        if (key.isEmpty() || value.isEmpty()) {
            throw ProvisionParseError.MalformedLine(line)
        }
        if (value.startsWith(QUOTE) && value.endsWith(QUOTE) && value.length >= MIN_QUOTED_LEN) {
            fields[key] = value.substring(1, value.length - 1)
        } else {
            val parsed = value.toIntOrNull() ?: throw ProvisionParseError.MalformedLine(line)
            integerFields[key] = parsed
        }
    }
    val schemaVersion = integerFields[ProvisionKeys.SCHEMA_VERSION]
        ?: throw ProvisionParseError.MissingField(ProvisionKeys.SCHEMA_VERSION)
    if (schemaVersion != PROVISION_SCHEMA_VERSION) {
        throw ProvisionParseError.UnsupportedSchemaVersion(got = schemaVersion)
    }
    val hostName = fields[ProvisionKeys.HOST_NAME]
        ?: throw ProvisionParseError.MissingField(ProvisionKeys.HOST_NAME)
    val peerId = fields[ProvisionKeys.PEER_ID]
        ?: throw ProvisionParseError.MissingField(ProvisionKeys.PEER_ID)
    val bondKey = decodeHex32(fields, ProvisionKeys.BOND_KEY_HEX)
    val seed = decodeHex32(fields, ProvisionKeys.PHONE_SIGNING_KEY_HEX)
    val pubkey = decodeHex32(fields, ProvisionKeys.PHONE_PUBKEY_HEX)
    val createdAt = fields[ProvisionKeys.CREATED_AT]
        ?: throw ProvisionParseError.MissingField(ProvisionKeys.CREATED_AT)
    return ProvisionPackage(
        schemaVersion = schemaVersion,
        hostName = hostName,
        peerId = peerId,
        bondKey = bondKey,
        phoneSigningKeySeed = seed,
        phonePubkey = pubkey,
        createdAt = createdAt,
    )
}

/** Serialize a package back to the canonical TOML body (used by [BondStore]). */
public fun serializeProvisionPackage(pkg: ProvisionPackage): String {
    val sb = StringBuilder()
    sb.append(ProvisionKeys.SCHEMA_VERSION).append(SERIALIZE_INT_SEP).append(pkg.schemaVersion)
        .append(NEWLINE)
    sb.append(ProvisionKeys.HOST_NAME).appendQuoted(pkg.hostName)
    sb.append(ProvisionKeys.PEER_ID).appendQuoted(pkg.peerId)
    sb.append(ProvisionKeys.BOND_KEY_HEX).appendQuoted(encodeHex(pkg.bondKey))
    sb.append(ProvisionKeys.PHONE_SIGNING_KEY_HEX).appendQuoted(encodeHex(pkg.phoneSigningKeySeed))
    sb.append(ProvisionKeys.PHONE_PUBKEY_HEX).appendQuoted(encodeHex(pkg.phonePubkey))
    sb.append(ProvisionKeys.CREATED_AT).appendQuoted(pkg.createdAt)
    return sb.toString()
}

private fun StringBuilder.appendQuoted(value: String) {
    append(SERIALIZE_STRING_SEP).append(QUOTE).append(value).append(QUOTE).append(NEWLINE)
}

private fun decodeHex32(fields: Map<String, String>, key: String): ByteArray {
    val raw = fields[key] ?: throw ProvisionParseError.MissingField(key)
    if (raw.length != PROVISION_HEX32_LEN) {
        throw ProvisionParseError.MalformedHex(field = key, length = raw.length)
    }
    val out = ByteArray(PROVISION_KEY_BYTES)
    for (i in 0 until PROVISION_KEY_BYTES) {
        val hi = hexNibble(raw[HEX_PAIR_STRIDE * i])
        val lo = hexNibble(raw[HEX_PAIR_STRIDE * i + 1])
        if (hi < 0 || lo < 0) {
            throw ProvisionParseError.MalformedHex(field = key, length = raw.length)
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
