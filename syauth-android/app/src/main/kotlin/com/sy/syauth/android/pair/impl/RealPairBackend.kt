// DEV-001 (CDM pivot): real Android-Bluetooth-backed [PairBackend].
//
// The PHONE no longer drives `BluetoothLeScanner.startScan` — Samsung
// One UI on Galaxy S25 Ultra (Android 15) rejects unprivileged
// scanners with `SecurityException: BLUETOOTH_PRIVILEGED required`,
// even when `BLUETOOTH_SCAN(neverForLocation)` is granted (full
// diagnostic in `specs/auto/RUN-2026-05-17T07-56-16Z.md` "DEV-001
// second e2e attempt: BLE diagnostic"). Instead the backend hands
// scanning to `CompanionDeviceManager.associate(AssociationRequest)`
// carrying a `BluetoothLeDeviceFilter` keyed on the desktop's
// rotating pair-mode UUID; the OS runs the scan under system
// privileges and presents the user with a system-rendered device
// picker. This is still SPEC §3.2 D8 in force ("the phone scans and
// connects"): the OS scanner is the phone's scanner, just routed
// through the Android-blessed companion API.
//
// Direction is unchanged: the DESKTOP advertises, the PHONE scans +
// connects via the OS picker. Matches DEV-003's unlock-channel
// direction for end-to-end consistency.
//
// Flow mapped onto the JOURNEY-DEV-001 phases:
//
//   Phase 1 — startScan(): launch a CDM associate request with two
//             `BluetoothLeDeviceFilter` slots (current minute +
//             previous minute, for 1-min skew absorption). The OS
//             shows its picker; on user pick, the CDM-launcher
//             callback in `MainActivity` drives `onPeerPicked` into
//             the ViewModel.
//   Phase 2 — pickPeer(peer): trigger the OS bond via
//             `BluetoothDevice.createBond()`. The
//             [PairingBroadcastReceiver] registered at construction
//             enforces `PAIRING_VARIANT_PASSKEY_CONFIRMATION` (value
//             `2` per AOSP).
//   Phase 3 — awaitLescResult(): block on a `CompletableDeferred`
//             that the bond-state broadcast receiver resolves when
//             the device reaches `BOND_BONDED`. After bonded, open
//             a fresh GATT client to the (now-bonded) device, write
//             the phone's Keystore-minted pubkey to the
//             `phone-pubkey` characteristic, read the desktop's
//             pubkey from `host-pubkey`, derive bond_key via
//             [bondKeyFromPubkeys], and return
//             [LescResult.Bonded].
//
// All Android platform dependencies live behind small interfaces so
// the JVM/Robolectric unit tests in `app/src/test/.../pair/`
// substitute fakes without standing up a real BLE stack.
package com.sy.syauth.android.pair.impl

import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothDevice
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.util.Log
import com.sy.syauth.android.pair.api.LescResult
import com.sy.syauth.android.pair.api.PairBackend
import com.sy.syauth.android.pair.api.PeerHandle
import com.sy.syauth.android.pair.api.PickPeerResult
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.runBlocking
import java.util.UUID
import java.util.concurrent.atomic.AtomicReference

/** Failure reason surfaced when the Bluetooth adapter is missing. */
public const val ADAPTER_MISSING_REASON: String = "no Bluetooth adapter available on this device"

/** Failure reason surfaced when the runtime BLUETOOTH_CONNECT grant is missing. */
public const val PERMISSIONS_MISSING_REASON: String = "BLUETOOTH_CONNECT runtime permission not granted"

/** Logcat tag the backend writes its life-cycle spans to. */
public const val REAL_PAIR_BACKEND_LOG_TAG: String = "syauth.pair"

/** Number of seconds in one wall-clock minute. */
internal const val PAIR_SECONDS_PER_MINUTE: Long = 60L

/** Length in bytes of an Ed25519 pubkey shipped across the pair service. */
public const val PAIR_PUBKEY_LEN: Int = 32

/** Length in bytes of the derived 16-byte rotating session UUID. */
internal const val PAIR_UUID_BYTE_LEN: Int = 16

/** Bond key length (32 bytes) returned by the post-bond pubkey-exchange step. */
public const val PAIR_BOND_KEY_LEN: Int = 32

/**
 * Value of `BluetoothDevice.TRANSPORT_LE` per the AOSP source. The
 * symbol is `@hide` for third-party apps but the int value (2) is
 * documented and stable across API levels; we pin it here so the
 * createBond reflection call doesn't need to drag in the SDK
 * constant via another `@hide` API.
 */
internal const val BLUETOOTH_TRANSPORT_LE: Int = 2

/** Placeholder surfaced as the LESC "6-digit code" until the broadcast lands. */
public const val LESC_PENDING_PLACEHOLDER: String = "(awaiting OS pairing request)"

/** Surfaced as the [LescResult.Failed] reason when the bond never resolves. */
public const val LESC_BOND_NEVER_RESOLVED_REASON: String = "OS bond never reached BOND_BONDED"

/** Surfaced when `pickPeer` cannot derive a [BluetoothDevice] from the peer id. */
public const val PEER_LOOKUP_FAILED_REASON: String = "could not look up BluetoothDevice for the chosen peer"

/** Surfaced when the post-bond exchange step has no GATT seam wired (test fixture / pre-pair). */
public const val GATT_EXCHANGE_MISSING_REASON: String = "no GATT exchange wired"

/**
 * Surfaced when no [KeystoreKeyGenerator] has been injected (pre-Tiramisu
 * device or test wiring). SPEC §3.2 D6 forbids shipping a zero-pubkey
 * across the wire; the pair flow aborts before any GATT bytes flow.
 */
public const val KEYSTORE_UNAVAILABLE_REASON: String =
    "Keystore Ed25519 generator unavailable on this device"

/** Prefix attached to the failure reason when [KeystoreKeyGenerator.generate] throws. */
public const val KEYSTORE_MINT_FAILED_PREFIX: String = "Keystore Ed25519 mint failed: "

/** Stable prefix for the per-address Keystore alias used by the unlock-time signer. */
public const val KEYSTORE_ALIAS_PREFIX: String = "syauth.ed25519."

// ---------------------------------------------------------------------------
// Seam interfaces — every Android platform dependency lives behind one of
// these so the Robolectric tests can substitute deterministic fakes.
// ---------------------------------------------------------------------------

/**
 * CDM-driven companion scanner seam. Production wraps
 * `CompanionDeviceManager.associate(AssociationRequest, ...)` carrying
 * a `BluetoothLeDeviceFilter(serviceUuid = rotating_pair_uuid)`; the
 * OS runs the scan under system privileges and presents a
 * system-rendered device picker. Tests inject a fake that captures
 * the requested service-UUID filter set and synchronously drives
 * `onPicked` / `onFailed`.
 */
public interface PairCompanionScanner {
    /**
     * Launch the CDM device picker for any peer advertising one of
     * the rotating pair-mode `serviceUuids`. The OS handles the
     * radio-level scan; on user pick `onPicked` fires with the
     * picked device's MAC address and friendly name. On every
     * failure mode (cancel, timeout, system error) `onFailed` fires
     * with a single-line reason string. Exactly one of the callbacks
     * will be invoked per call.
     */
    public fun associate(
        serviceUuids: List<UUID>,
        onPicked: (deviceAddress: String, deviceName: String?) -> Unit,
        onFailed: (reason: String) -> Unit,
    )
}

/**
 * Minimal abstraction over the post-bond pubkey-exchange path the
 * production backend uses. Production wraps `BluetoothDevice
 * .connectGatt(...)`; tests inject a fake that returns a deterministic
 * `host-pubkey` payload.
 */
public interface PairGattExchange {
    /**
     * Connect to `address` (already bonded), write `phonePubkey` to
     * the desktop's `phone-pubkey` characteristic, read the
     * `host-pubkey` characteristic, and return its bytes. Throws on
     * any underlying failure; the backend maps the throw to a
     * `LescResult.Failed`.
     */
    public fun exchangePubkeys(address: String, phonePubkey: ByteArray): ByteArray
}

/**
 * Source of wall-clock seconds the backend uses to compute the
 * current minute. Production injects `System::currentTimeMillis / 1000`;
 * tests pin a fixed value.
 */
public fun interface PairClock {
    /** Return the current unix-epoch seconds. */
    public fun nowEpochSeconds(): Long
}

/**
 * UniFFI surface forwarder. Production wires
 * `uniffi.syauth_mobile.sessionUuidForBond`; tests inject a pure
 * lookup that produces deterministic bytes from `(bondKey, minute)`.
 */
public fun interface PairSessionUuidLookup {
    public fun lookup(bondKey: ByteArray, minute: Long): ByteArray
}

/**
 * Helper that derives the shared `bond_key` from the two exchanged
 * Ed25519 pubkeys. Mirrors `syauth_core::bond_key_from_pubkeys`
 * byte-for-byte. Production wires the UniFFI forwarder; tests
 * inject a deterministic stub.
 */
public fun interface PairBondKeyDeriver {
    public fun derive(hostPubkey: ByteArray, phonePubkey: ByteArray): ByteArray
}

/**
 * Receiver registrar — abstracted so a JVM-only test can drive the
 * `onReceive` callback directly without standing up a real Android
 * `Context`. Production wires `Context::registerReceiver` /
 * `Context::unregisterReceiver`.
 */
public interface ReceiverRegistrar {
    public fun register(receiver: BroadcastReceiver, filter: IntentFilter)
    public fun unregister(receiver: BroadcastReceiver)
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

/**
 * Number of forward minute slots the phone's CDM scan filter covers
 * past the current wall-clock minute. Matches
 * `PAIR_ADVERTISE_ACCEPT_WINDOW` on the desktop (300s ÷ 60s = 5min):
 * if the user opens the CDM picker right after the desktop's pair
 * window begins, the phone's filter has to stay valid through all 5
 * desktop rotations. We include the next 4 future slots (current +
 * 4 future = 5 forward slots = 5 minutes of coverage) plus 1
 * previous slot for ±1-minute clock skew.
 */
public const val PAIR_FILTER_FUTURE_SLOTS: Int = 4

/**
 * Compute the pair-mode discovery UUIDs the phone is willing to
 * accept on CDM. The set is `{N-1, N, N+1, ..., N+PAIR_FILTER_FUTURE_SLOTS}`
 * where `N = nowEpochSeconds / 60`. The previous slot is included for
 * ±1-minute clock skew between phone and desktop; the future slots
 * cover the desktop's rotating advertisement across its 5-minute
 * accept window so the phone's filter stays valid even when the user
 * taps "Pair with computer" shortly before a minute boundary or
 * keeps the picker open while the desktop rotates. Pure function for
 * test determinism — `nowEpochSeconds` is injected.
 */
public fun pairModeUuidsFor(
    nowEpochSeconds: Long,
    sessionUuidLookup: PairSessionUuidLookup,
): List<UUID> {
    val zeroBondKey = ByteArray(PAIR_BOND_KEY_LEN) // no bond exists yet at pair time
    val currentMinute = nowEpochSeconds / PAIR_SECONDS_PER_MINUTE
    val previousMinute = currentMinute - 1L
    val out = ArrayList<UUID>(2 + PAIR_FILTER_FUTURE_SLOTS)
    out.add(pairUuidFromBytes(sessionUuidLookup.lookup(zeroBondKey, previousMinute)))
    out.add(pairUuidFromBytes(sessionUuidLookup.lookup(zeroBondKey, currentMinute)))
    for (offset in 1..PAIR_FILTER_FUTURE_SLOTS) {
        out.add(pairUuidFromBytes(sessionUuidLookup.lookup(zeroBondKey, currentMinute + offset)))
    }
    return out
}

/** Wrap 16 big-endian bytes into a `java.util.UUID`. */
internal fun pairUuidFromBytes(bytes: ByteArray): UUID {
    if (bytes.size != PAIR_UUID_BYTE_LEN) {
        return UUID(0L, 0L)
    }
    var msb = 0L
    var lsb = 0L
    for (i in 0 until PAIR_UUID_BYTE_LEN / 2) {
        msb = (msb shl PAIR_UUID_BYTE_SHIFT) or (bytes[i].toLong() and PAIR_UUID_BYTE_MASK)
    }
    for (i in PAIR_UUID_BYTE_LEN / 2 until PAIR_UUID_BYTE_LEN) {
        lsb = (lsb shl PAIR_UUID_BYTE_SHIFT) or (bytes[i].toLong() and PAIR_UUID_BYTE_MASK)
    }
    return UUID(msb, lsb)
}

/** Bits-per-byte for the UUID assembly. */
internal const val PAIR_UUID_BYTE_SHIFT: Int = 8

/** Mask to widen a signed byte to its unsigned long representation. */
internal const val PAIR_UUID_BYTE_MASK: Long = 0xFFL

// ---------------------------------------------------------------------------
// Production [PairBackend].
// ---------------------------------------------------------------------------

/**
 * DEV-001 (CDM pivot) production [PairBackend].
 *
 * Constructor takes one explicit seam per Android platform dependency
 * so the Robolectric tests can substitute fakes. Production wires the
 * real seams via [PairingViewModelFactoryHolder] in `MainActivity`.
 *
 * Lifecycle:
 * 1. `init { ... }` registers both the
 *    [PairingBroadcastReceiver] (gate the OS-level
 *    `ACTION_PAIRING_REQUEST` variant) and a
 *    [BondStateBroadcastReceiver] (resolve the
 *    `CompletableDeferred<LescResult>` once the OS reaches
 *    `BOND_BONDED`).
 * 2. `startScan()` launches the [companionScanner] with the two
 *    pair-mode UUIDs (slot N + slot N-1). The OS shows its picker;
 *    on pick, the scanner's `onPicked` callback invokes
 *    [onPeerPickedCallback] which drives the ViewModel directly to
 *    `pickPeer(peer)` and then `LescNegotiating`.
 * 3. `pickPeer(peer)` triggers `BluetoothDevice.createBond()`.
 * 4. `awaitLescResult()` blocks on the deferred. On success it
 *    performs the post-bond pubkey exchange via [gattExchange] and
 *    returns [LescResult.Bonded].
 * 5. `cleanup()` unregisters both receivers; called from the
 *    ViewModel's `onCleared()`.
 */
public class RealPairBackend(
    private val context: Context,
    private val adapter: BluetoothAdapter?,
    private val companionScanner: PairCompanionScanner?,
    private val gattExchange: PairGattExchange?,
    private val pairingReceiverRegistrar: ReceiverRegistrar,
    private val bondStateReceiverRegistrar: ReceiverRegistrar,
    private val clock: PairClock,
    private val sessionUuidLookup: PairSessionUuidLookup,
    private val bondKeyDeriver: PairBondKeyDeriver,
    private val keystoreKeyGenerator: KeystoreKeyGenerator? = null,
    private val onLescResultCallback: AtomicReference<(LescResult) -> Unit> = AtomicReference { _ -> },
    private val onPeerPickedCallback: AtomicReference<(PeerHandle) -> Unit> = AtomicReference { _ -> },
    private val onScanFailedCallback: AtomicReference<(String) -> Unit> = AtomicReference { _ -> },
) : PairBackend {

    /**
     * Latest CDM-picked peers. Carries the one device the user chose
     * via the OS picker; `pickPeer` consumes it.
     */
    public val foundPeers: MutableList<PeerHandle> = mutableListOf()

    /**
     * Last-seen 6-digit code from the pairing-request broadcast.
     * Initialized to [LESC_PENDING_PLACEHOLDER]; the receiver
     * overwrites it once the OS fires `ACTION_PAIRING_REQUEST` with
     * variant = [PAIRING_VARIANT_PASSKEY_CONFIRMATION].
     */
    @Volatile
    public var lastPairingCode: String = LESC_PENDING_PLACEHOLDER
        private set

    /**
     * Address of the peer the operator picked. Held so the bond-state
     * receiver can derive the device handle on `BOND_BONDED`.
     */
    @Volatile
    private var pickedAddress: String? = null

    /**
     * Friendly name of the peer the operator picked. Surfaced in the
     * eventual [LescResult.Bonded].
     */
    @Volatile
    private var pickedName: String = ""

    /** Resolved by the bond-state receiver. */
    private val lescResultDeferred: CompletableDeferred<LescResult> = CompletableDeferred()

    /** Pairing-request receiver registered at init. */
    private val pairingReceiver: PairingBroadcastReceiver = PairingBroadcastReceiver(
        onAccept = { passkey -> lastPairingCode = passkey.toString().padStart(LESC_CODE_DIGITS, '0') },
        onReject = { variant ->
            Log.w(REAL_PAIR_BACKEND_LOG_TAG, "pairing variant rejected variant=$variant")
            lescResultDeferred.complete(LescResult.Failed("OS pairing variant rejected: $variant"))
        },
    )

    /** Bond-state receiver registered at init. */
    private val bondStateReceiver: BondStateBroadcastReceiver = BondStateBroadcastReceiver(
        onBonded = ::onBondedFromReceiver,
        onFailed = { reason -> lescResultDeferred.complete(LescResult.Failed(reason)) },
    )

    /** True after [cleanup] has run; further public calls become no-ops. */
    @Volatile
    private var cleanedUp: Boolean = false

    init {
        registerReceivers()
    }

    /**
     * Install the production callback that fires whenever the
     * backend resolves a [LescResult]. The factory (in `MainActivity`)
     * calls this once with `viewModel::onLescResult` so the ViewModel
     * advances the state machine without the backend depending on the
     * ViewModel type.
     */
    public fun setOnLescResultCallback(cb: (LescResult) -> Unit) {
        onLescResultCallback.set(cb)
    }

    /**
     * Install the production callback that fires when the CDM picker
     * resolves with a chosen peer. The factory (in `MainActivity`)
     * calls this once with `viewModel::onPeerPicked` so the
     * ViewModel transitions Scanning → LescNegotiating.
     */
    public fun setOnPeerPickedCallback(cb: (PeerHandle) -> Unit) {
        onPeerPickedCallback.set(cb)
    }

    /**
     * Install the production callback that fires when the CDM picker
     * fails (user cancel, system error). The factory wires this to
     * `viewModel::onCancelTapped` so Scanning → Idle.
     */
    public fun setOnScanFailedCallback(cb: (String) -> Unit) {
        onScanFailedCallback.set(cb)
    }

    private fun registerReceivers() {
        pairingReceiverRegistrar.register(
            pairingReceiver,
            IntentFilter(BluetoothDevice.ACTION_PAIRING_REQUEST),
        )
        bondStateReceiverRegistrar.register(
            bondStateReceiver,
            IntentFilter(BluetoothDevice.ACTION_BOND_STATE_CHANGED),
        )
    }

    override fun startScan() {
        if (cleanedUp) return
        val scanner = companionScanner ?: return
        val nowSeconds = clock.nowEpochSeconds()
        val slotUuids = pairModeUuidsFor(nowSeconds, sessionUuidLookup)
        lastFilterUuids = slotUuids
        scanner.associate(
            slotUuids,
            { address, name ->
                val display = name ?: address
                val handle = PeerHandle(id = address, name = display)
                synchronized(foundPeers) {
                    if (foundPeers.none { it.id == address }) {
                        foundPeers.add(handle)
                    }
                }
                onPeerPickedCallback.get().invoke(handle)
            },
            { reason ->
                Log.w(REAL_PAIR_BACKEND_LOG_TAG, "CDM associate failed: $reason")
                onScanFailedCallback.get().invoke(reason)
            },
        )
    }

    /** No-op: CDM owns the picker lifecycle. Cancelling the dialog is the user's action. */
    override fun stopScan() {
        // CDM-driven scan: the OS picker owns its own lifecycle. The
        // user dismisses it; no app-side stop API exists.
    }

    /**
     * Slot UUIDs the backend last requested via CDM. Exposed so tests
     * can assert the (current, previous) inclusion without driving a
     * real CDM session.
     */
    @Volatile
    public var lastFilterUuids: List<UUID> = emptyList()
        internal set

    override fun pickPeer(peer: PeerHandle): PickPeerResult {
        val a = adapter ?: return PickPeerResult.Failed(ADAPTER_MISSING_REASON)
        if (cleanedUp) return PickPeerResult.Failed("backend has been cleaned up")
        val device = runCatching { a.getRemoteDevice(peer.id) }.getOrElse {
            return PickPeerResult.Failed(PEER_LOOKUP_FAILED_REASON)
        }
        pickedAddress = peer.id
        pickedName = peer.name
        // Force the LE transport for bonding. `createBond()` without
        // a transport defaults to `BT_TRANSPORT_AUTO`, which the
        // Android stack resolves to BR/EDR on dual-mode peers — the
        // desktop's BlueZ adapter advertises both LE and BR/EDR, so
        // the system picks Classic SSP. Our pair backend's BlueZ
        // agent only handles LESC numeric comparison over LE; the
        // BR/EDR SSP request has no handler on the desktop and the
        // bond times out with HCI_ERR_AUTH_FAILURE.
        // `createBond(int)` is `@hide` for third-party apps but
        // stable across API levels (the int argument has been on
        // `BluetoothDevice` since API 23); reach it via reflection.
        // `BluetoothDevice.TRANSPORT_LE` is the documented constant
        // value `2`.
        val bondState = runCatching { device.bondState }.getOrElse { -1 }
        Log.i(
            REAL_PAIR_BACKEND_LOG_TAG,
            "pickPeer addr=${peer.id} bondState=$bondState type=${runCatching { device.type }.getOrElse { -1 }}",
        )
        // Existing OS-level bond, but app-level Phase 4 (GATT pubkey
        // exchange + bond.json write) may never have completed — happens
        // after a prior run reached BOND_BONDED at the OS but deadlocked
        // before exchangePubkeys. Skip createBond and drive Phase 4
        // directly off the existing bond.
        if (bondState == BluetoothDevice.BOND_BONDED) {
            Log.i(REAL_PAIR_BACKEND_LOG_TAG, "already bonded; skipping createBond, driving Phase 4")
            Thread({ runPostBondExchange(peer.id) }, "syauth-pair-gatt").start()
            return PickPeerResult.LescStarted(lastPairingCode)
        }
        val startedResult: Result<Boolean> = runCatching {
            val method = device.javaClass.getMethod("createBond", Int::class.javaPrimitiveType)
            method.invoke(device, BLUETOOTH_TRANSPORT_LE) as? Boolean ?: false
        }
        val started = startedResult.getOrElse { err ->
            Log.w(REAL_PAIR_BACKEND_LOG_TAG, "createBond(TRANSPORT_LE) threw", err)
            false
        }
        Log.i(REAL_PAIR_BACKEND_LOG_TAG, "createBond(TRANSPORT_LE) returned $started")
        return if (started) {
            PickPeerResult.LescStarted(lastPairingCode)
        } else {
            PickPeerResult.Failed(
                "BluetoothDevice.createBond(TRANSPORT_LE) refused (bondState=$bondState)",
            )
        }
    }

    override fun awaitLescResult(): LescResult =
        runBlocking { lescResultDeferred.await() }

    /**
     * Tear down the receivers and any active scan. Called by the
     * ViewModel from `onCleared()` so the backend does not leak
     * receivers across pair attempts.
     */
    public fun cleanup() {
        if (cleanedUp) return
        cleanedUp = true
        runCatching { pairingReceiverRegistrar.unregister(pairingReceiver) }
        runCatching { bondStateReceiverRegistrar.unregister(bondStateReceiver) }
        if (!lescResultDeferred.isCompleted) {
            lescResultDeferred.complete(LescResult.Failed("backend cleanup"))
        }
    }

    /**
     * DEV-002 hook: mint a fresh Ed25519 keypair under the Keystore.
     * Returns `null` ONLY when no generator has been injected (test
     * wiring or pre-Tiramisu device). On a wired generator, any
     * [KeystoreKeygenError] propagates to the caller so
     * [runPostBondExchange] can surface a typed `LescResult.Failed`
     * rather than ship a zero-pubkey across the wire (SPEC §3.2 D6
     * forbids shipping unsigned material).
     */
    @Throws(KeystoreKeygenError::class)
    public fun mintKeystoreEd25519(alias: String): KeystoreEd25519KeyMaterial? {
        val gen = keystoreKeyGenerator
        if (gen == null) {
            Log.w(REAL_PAIR_BACKEND_LOG_TAG, "mintKeystoreEd25519: keystoreKeyGenerator is null")
            return null
        }
        val mat = gen.generate(alias)
        Log.i(
            REAL_PAIR_BACKEND_LOG_TAG,
            "mintKeystoreEd25519 ok alias=$alias strongBox=${mat.strongBoxBacked} pubkeyLen=${mat.pubkey.size}",
        )
        return mat
    }

    /**
     * Called by the bond-state receiver when the OS reaches
     * `BOND_BONDED`. Drives the post-bond pubkey exchange and
     * resolves the [CompletableDeferred] with the eventual
     * [LescResult].
     *
     * Runs the GATT exchange off the main thread because the
     * receiver's `onReceive` callback fires on the main looper.
     * `AndroidPairGattExchange.exchangePubkeys` blocks on
     * `CountDownLatch.await` waiting for `BluetoothGattCallback`
     * dispatches, and those callbacks also default to the main
     * thread — running the exchange inline deadlocks the
     * service-discovery + characteristic-read/write path. The
     * 6-second `l2c_link_timeout: All channels closed` we saw
     * post-`BOND_BONDED` in the 2026-05-17 run was exactly this:
     * the link came up encrypted, but the main thread was wedged
     * inside `exchangePubkeys` so no GATT traffic ever flowed, and
     * BlueZ's idle timeout dropped the link before service discovery
     * resolved.
     */
    private fun onBondedFromReceiver(address: String) {
        val picked = pickedAddress
        if (picked != null && picked != address) {
            // Bond resolved for a device we did not pick. Ignore.
            return
        }
        Thread({ runPostBondExchange(address) }, "syauth-pair-gatt").start()
    }

    /**
     * Runs the post-bond pubkey exchange and resolves
     * [lescResultDeferred] with the outcome. Invoked from the worker
     * thread spawned by [onBondedFromReceiver]; exposed as `internal`
     * so the Robolectric runtime test can drive the failure-surface
     * matrix synchronously without spinning up a real GATT stack.
     */
    internal fun runPostBondExchange(address: String) {
        val exchange = gattExchange
        if (exchange == null) {
            lescResultDeferred.complete(LescResult.Failed(GATT_EXCHANGE_MISSING_REASON))
            return
        }
        val alias = "$KEYSTORE_ALIAS_PREFIX${address.replace(":", "")}"
        val material: KeystoreEd25519KeyMaterial = try {
            mintKeystoreEd25519(alias)
                ?: run {
                    // SPEC §3.2 D6: no Keystore generator wired (pre-Tiramisu
                    // device or test seam). Refuse to ship a zero-pubkey;
                    // surface a typed failure to the ViewModel.
                    Log.w(REAL_PAIR_BACKEND_LOG_TAG, KEYSTORE_UNAVAILABLE_REASON)
                    lescResultDeferred.complete(LescResult.Failed(KEYSTORE_UNAVAILABLE_REASON))
                    return
                }
        } catch (e: KeystoreKeygenError) {
            val reason = "$KEYSTORE_MINT_FAILED_PREFIX${e.message ?: e::class.java.simpleName}"
            Log.w(REAL_PAIR_BACKEND_LOG_TAG, reason, e)
            lescResultDeferred.complete(LescResult.Failed(reason))
            return
        }
        val phonePubkey = material.pubkey
        val hostPubkey = runCatching { exchange.exchangePubkeys(address, phonePubkey) }.getOrElse {
            val reason = "pair-service exchange failed: ${it.message ?: it::class.java.simpleName}"
            Log.w(REAL_PAIR_BACKEND_LOG_TAG, reason, it)
            lescResultDeferred.complete(LescResult.Failed(reason))
            return
        }
        if (hostPubkey.size != PAIR_PUBKEY_LEN) {
            lescResultDeferred.complete(LescResult.Failed("host pubkey wrong length: ${hostPubkey.size}"))
            return
        }
        val bondKey = bondKeyDeriver.derive(hostPubkey, phonePubkey)
        if (bondKey.size != PAIR_BOND_KEY_LEN) {
            lescResultDeferred.complete(LescResult.Failed("bond_key wrong length: ${bondKey.size}"))
            return
        }
        val result = LescResult.Bonded(
            bondKey = bondKey,
            peerName = pickedName.ifEmpty { address },
            keystoreAlias = material.alias,
            phonePubkey = phonePubkey,
        )
        Log.i(REAL_PAIR_BACKEND_LOG_TAG, "post-bond exchange complete addr=$address")
        lescResultDeferred.complete(result)
        onLescResultCallback.get().invoke(result)
    }

    private companion object {
        /** Number of digits in the LESC numeric-comparison code. */
        const val LESC_CODE_DIGITS: Int = 6
    }
}

/**
 * Broadcast receiver listening for `ACTION_BOND_STATE_CHANGED`.
 * On `BOND_BONDED`, invokes [onBonded] with the device address;
 * on `BOND_NONE` after a prior `BOND_BONDING`, invokes [onFailed].
 */
public class BondStateBroadcastReceiver(
    private val onBonded: (String) -> Unit,
    private val onFailed: (String) -> Unit,
) : BroadcastReceiver() {

    /** Previous bond state observed for the picked device. */
    @Volatile
    private var lastState: Int = BluetoothDevice.BOND_NONE

    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != BluetoothDevice.ACTION_BOND_STATE_CHANGED) return
        val newState = intent.getIntExtra(BluetoothDevice.EXTRA_BOND_STATE, BluetoothDevice.BOND_NONE)
        val prevState = intent.getIntExtra(BluetoothDevice.EXTRA_PREVIOUS_BOND_STATE, lastState)
        val device: BluetoothDevice? = intent.getParcelableExtra(BluetoothDevice.EXTRA_DEVICE)
        Log.i(REAL_PAIR_BACKEND_LOG_TAG, "bond-state: prev=$prevState new=$newState addr=${device?.address}")
        lastState = newState
        when (newState) {
            BluetoothDevice.BOND_BONDED -> {
                val addr = device?.address ?: return
                onBonded(addr)
            }
            BluetoothDevice.BOND_NONE -> {
                if (prevState == BluetoothDevice.BOND_BONDING) {
                    onFailed("OS bond cancelled or failed (BOND_BONDING -> BOND_NONE)")
                }
            }
            else -> Unit // BOND_BONDING is a transient state we do not surface
        }
    }
}
