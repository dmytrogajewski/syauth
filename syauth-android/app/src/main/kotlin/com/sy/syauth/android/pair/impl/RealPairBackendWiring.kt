// DEV-001 (re-march): production wrappers around the Android
// Bluetooth platform surface used by [RealPairBackend].
//
// Every wrapper here implements one of the [RealPairBackend]
// constructor's seam interfaces with a real Android dependency. The
// Robolectric tests do NOT touch this file — they inject hand-rolled
// fakes directly into [RealPairBackend].
package com.sy.syauth.android.pair.impl

import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothGatt
import android.bluetooth.BluetoothGattCallback
import android.bluetooth.BluetoothGattCharacteristic
import android.content.BroadcastReceiver
import android.content.Context
import android.content.IntentFilter
import java.security.MessageDigest
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import uniffi.syauth_mobile.sessionUuidForBond

/**
 * Fixed UUID of the desktop's transient pair service. Byte-identical
 * to the Rust constant `SYAUTH_PAIR_SERVICE_UUID` in
 * `crates/syauth-transport/src/bluez.rs`.
 */
public val SYAUTH_PAIR_SERVICE_UUID: UUID = UUID.fromString("5a4e8e3c-1c4c-4a17-9c81-d518a55a0101")

/**
 * Characteristic holding the desktop's 32-byte Ed25519 host pubkey.
 * Byte-identical to the Rust constant
 * `SYAUTH_PAIR_HOST_PUBKEY_CHAR_UUID`.
 */
public val SYAUTH_PAIR_HOST_PUBKEY_CHAR_UUID: UUID = UUID.fromString("5a4e8e3c-1c4c-4a17-9c81-d518a55a0102")

/**
 * Characteristic the phone writes its 32-byte Ed25519 pubkey to.
 * Byte-identical to the Rust constant
 * `SYAUTH_PAIR_PHONE_PUBKEY_CHAR_UUID`.
 */
public val SYAUTH_PAIR_PHONE_PUBKEY_CHAR_UUID: UUID = UUID.fromString("5a4e8e3c-1c4c-4a17-9c81-d518a55a0103")

/** Default GATT-exchange wait window for service discovery + char read/write. */
public const val PAIR_GATT_EXCHANGE_TIMEOUT_SECS: Long = 30L

/**
 * Production [ReceiverRegistrar] wrapping
 * `Context.registerReceiver` / `Context.unregisterReceiver`.
 */
public class ContextReceiverRegistrar(private val context: Context) : ReceiverRegistrar {
    override fun register(receiver: BroadcastReceiver, filter: IntentFilter) {
        context.registerReceiver(receiver, filter)
    }
    override fun unregister(receiver: BroadcastReceiver) {
        context.unregisterReceiver(receiver)
    }
}

/**
 * Production [PairClock] wrapping the system wall-clock.
 */
public class SystemPairClock : PairClock {
    override fun nowEpochSeconds(): Long = System.currentTimeMillis() / MILLIS_PER_SECOND

    private companion object {
        const val MILLIS_PER_SECOND: Long = 1_000L
    }
}

/**
 * Production [PairSessionUuidLookup] delegating to UniFFI's
 * `sessionUuidForBond` — byte-identical to the Rust
 * `syauth_transport::session_uuid_for`.
 */
public class UniffiPairSessionUuidLookup : PairSessionUuidLookup {
    override fun lookup(bondKey: ByteArray, minute: Long): ByteArray =
        sessionUuidForBond(bondKey, minute)
}

/**
 * Production [PairBondKeyDeriver]. Mirrors
 * `syauth_core::bond_key_from_pubkeys` byte-for-byte:
 *
 *   bond_key = HKDF-SHA256(salt=None,
 *                           ikm = host_pubkey || phone_pubkey,
 *                           info = "syauth-bond-v1")[0..32]
 *
 * Implemented in pure JVM crypto so the runtime does not need to
 * cross the UniFFI boundary for this single derivation.
 */
public class HkdfPairBondKeyDeriver : PairBondKeyDeriver {

    override fun derive(hostPubkey: ByteArray, phonePubkey: ByteArray): ByteArray {
        val ikm = ByteArray(hostPubkey.size + phonePubkey.size)
        System.arraycopy(hostPubkey, 0, ikm, 0, hostPubkey.size)
        System.arraycopy(phonePubkey, 0, ikm, hostPubkey.size, phonePubkey.size)
        return hkdfSha256(ikm = ikm, info = BOND_HKDF_INFO_V1, length = PAIR_BOND_KEY_LEN)
    }

    private fun hkdfSha256(ikm: ByteArray, info: ByteArray, length: Int): ByteArray {
        val mac = javax.crypto.Mac.getInstance(HMAC_ALG)
        val zeroSalt = ByteArray(MessageDigest.getInstance(SHA256_ALG).digestLength)
        mac.init(javax.crypto.spec.SecretKeySpec(zeroSalt, HMAC_ALG))
        val prk = mac.doFinal(ikm)
        // expand: T(1) = HMAC(prk, info || 0x01); concat until length.
        val out = ByteArray(length)
        var written = 0
        var prev = ByteArray(0)
        var counter = 1
        while (written < length) {
            mac.init(javax.crypto.spec.SecretKeySpec(prk, HMAC_ALG))
            mac.update(prev)
            mac.update(info)
            mac.update(counter.toByte())
            prev = mac.doFinal()
            val take = minOf(prev.size, length - written)
            System.arraycopy(prev, 0, out, written, take)
            written += take
            counter += 1
        }
        return out
    }

    private companion object {
        const val HMAC_ALG: String = "HmacSHA256"
        const val SHA256_ALG: String = "SHA-256"
        val BOND_HKDF_INFO_V1: ByteArray = "syauth-bond-v1".toByteArray(Charsets.US_ASCII)
    }
}

/**
 * Production [PairGattExchange] wrapping
 * `BluetoothDevice.connectGatt(...)`. Used by [RealPairBackend] after
 * `BOND_BONDED` lands; opens a fresh GATT client connection to the
 * (now bonded) device, discovers the pair service, writes the phone
 * pubkey to the `phone-pubkey` characteristic, reads the
 * `host-pubkey` characteristic, and returns the 32 bytes.
 *
 * The implementation uses blocking `CountDownLatch`-driven callbacks
 * because the [PairBackend] surface is synchronous. The backend
 * already calls [PairGattExchange.exchangePubkeys] from a coroutine
 * pumped via `awaitLescResult` -> `runBlocking`, so the blocking is
 * intentional and bounded by [PAIR_GATT_EXCHANGE_TIMEOUT_SECS].
 */
public class AndroidPairGattExchange(
    private val context: Context,
    private val adapter: BluetoothAdapter,
) : PairGattExchange {

    override fun exchangePubkeys(address: String, phonePubkey: ByteArray): ByteArray {
        val device = adapter.getRemoteDevice(address)
        val servicesDiscovered = CountDownLatch(1)
        val writeDone = CountDownLatch(1)
        val readDone = CountDownLatch(1)
        val hostPubkey: AtomicReference<ByteArray?> = AtomicReference(null)
        val failure: AtomicReference<String?> = AtomicReference(null)

        val callback = object : BluetoothGattCallback() {
            override fun onConnectionStateChange(gatt: BluetoothGatt, status: Int, newState: Int) {
                if (newState == BluetoothGatt.STATE_CONNECTED) {
                    runCatching { gatt.discoverServices() }
                } else if (newState == BluetoothGatt.STATE_DISCONNECTED) {
                    if (hostPubkey.get() == null && failure.get() == null) {
                        failure.set("GATT disconnected before host-pubkey read")
                    }
                    servicesDiscovered.countDown()
                    writeDone.countDown()
                    readDone.countDown()
                }
            }

            override fun onServicesDiscovered(gatt: BluetoothGatt, status: Int) {
                servicesDiscovered.countDown()
            }

            override fun onCharacteristicWrite(
                gatt: BluetoothGatt,
                characteristic: BluetoothGattCharacteristic,
                status: Int,
            ) {
                if (characteristic.uuid == SYAUTH_PAIR_PHONE_PUBKEY_CHAR_UUID) {
                    if (status != BluetoothGatt.GATT_SUCCESS) {
                        failure.set("phone-pubkey write failed status=$status")
                    }
                    writeDone.countDown()
                }
            }

            override fun onCharacteristicRead(
                gatt: BluetoothGatt,
                characteristic: BluetoothGattCharacteristic,
                status: Int,
            ) {
                if (characteristic.uuid == SYAUTH_PAIR_HOST_PUBKEY_CHAR_UUID) {
                    if (status == BluetoothGatt.GATT_SUCCESS) {
                        hostPubkey.set(characteristic.value?.copyOf())
                    } else {
                        failure.set("host-pubkey read failed status=$status")
                    }
                    readDone.countDown()
                }
            }
        }

        val gatt = device.connectGatt(context, false, callback)
        try {
            if (!servicesDiscovered.await(PAIR_GATT_EXCHANGE_TIMEOUT_SECS, TimeUnit.SECONDS)) {
                throw RuntimeException("service-discovery timeout")
            }
            failure.get()?.let { throw RuntimeException(it) }
            val service = gatt.getService(SYAUTH_PAIR_SERVICE_UUID)
                ?: throw RuntimeException("pair service not present on bonded device")
            val phoneChar = service.getCharacteristic(SYAUTH_PAIR_PHONE_PUBKEY_CHAR_UUID)
                ?: throw RuntimeException("phone-pubkey characteristic missing")
            val hostChar = service.getCharacteristic(SYAUTH_PAIR_HOST_PUBKEY_CHAR_UUID)
                ?: throw RuntimeException("host-pubkey characteristic missing")
            phoneChar.value = phonePubkey
            if (!gatt.writeCharacteristic(phoneChar)) {
                throw RuntimeException("phone-pubkey writeCharacteristic refused")
            }
            if (!writeDone.await(PAIR_GATT_EXCHANGE_TIMEOUT_SECS, TimeUnit.SECONDS)) {
                throw RuntimeException("phone-pubkey write timeout")
            }
            failure.get()?.let { throw RuntimeException(it) }
            if (!gatt.readCharacteristic(hostChar)) {
                throw RuntimeException("host-pubkey readCharacteristic refused")
            }
            if (!readDone.await(PAIR_GATT_EXCHANGE_TIMEOUT_SECS, TimeUnit.SECONDS)) {
                throw RuntimeException("host-pubkey read timeout")
            }
            failure.get()?.let { throw RuntimeException(it) }
            return hostPubkey.get() ?: throw RuntimeException("host-pubkey read returned null")
        } finally {
            runCatching { gatt.disconnect() }
            runCatching { gatt.close() }
        }
    }
}
