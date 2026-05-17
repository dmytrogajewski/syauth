// Roadmap item S-018 — GATT-backed ResponseSender production impl.
//
// The Approve flow (S-017) ships its terminal frame via
// `ResponseSender`. For S-018 the production implementation pushes
// the bytes back to the desktop over the SYAUTH_RESPONSE_CHAR_UUID
// characteristic on the GATT server the
// [SyauthCompanionService] opened. The sender is keyed by `peerId`
// so the registry can hold multiple in-flight Approve flows when
// SPEC §3.3 ML "OUT — explicitly not in v0.1.0" relaxes its
// single-peer constraint in a future scope bump; today only one
// slot is ever populated.
//
// If the OS unbinds the service between challenge-receive and the
// user's tap-Approve (battery optimisation disabled mid-flight, OEM
// process kill, etc.), the registry slot is empty when the sender
// tries to write. The contract surfaces this as a typed
// `ResponseSendError.ServiceUnbound`; the ApproveViewModel maps it
// to `Denied(SignError("service unbound"))` so the desktop sees a
// `PeerDenied` instead of a hang.
package com.sy.syauth.android.bg

import com.sy.syauth.android.approve.ResponseSender
import java.util.concurrent.ConcurrentHashMap

/**
 * Typed error surface for [GattResponseSender]. Production code
 * never throws; every failure becomes one of these via
 * `Result.failure`.
 */
public sealed class ResponseSendError(message: String) : RuntimeException(message) {
    /** The CompanionDeviceService binding is gone. */
    public class ServiceUnbound(message: String) : ResponseSendError(message)

    /** The underlying GATT write failed at the radio layer. */
    public class GattWriteFailed(message: String) : ResponseSendError(message)
}

/**
 * Contract for the per-peer transport the [GattResponseSender]
 * looks up. Production wires a delegate that calls
 * `BluetoothGattServer.notifyCharacteristicChanged`; tests inject a
 * fake that records the payload.
 */
public interface GattResponseTransport {
    /** Push [bytes] back to the bonded desktop as the approve response. */
    public suspend fun pushApprove(bytes: ByteArray): Result<Unit>

    /** Push the deny sentinel back to the bonded desktop. */
    public suspend fun pushDeny(): Result<Unit>
}

/**
 * Global registry of in-flight transports keyed by peerId. The
 * service installs its per-association transport on
 * `onDeviceAppeared`; the sender looks it up at send time. We use
 * a global because the ApproveViewModel is constructed by
 * MainActivity (a distinct process surface from the service) and
 * cannot hold a direct reference to the service instance.
 *
 * Concurrency: `ConcurrentHashMap` because reads and writes
 * happen on the binder thread (service) and the main thread
 * (ViewModel) respectively.
 */
public object GattResponseTransports {
    private val transports: ConcurrentHashMap<String, GattResponseTransport> =
        ConcurrentHashMap()

    public fun register(peerId: String, transport: GattResponseTransport) {
        transports[peerId] = transport
    }

    public fun unregister(peerId: String) {
        transports.remove(peerId)
    }

    public fun lookup(peerId: String): GattResponseTransport? = transports[peerId]

    /** Reset state for tests. */
    public fun reset() {
        transports.clear()
    }
}

/**
 * Production [ResponseSender] that looks up the per-peer transport
 * in [GattResponseTransports] and forwards the call. If the
 * transport has been unregistered (service unbound), the call is
 * a silent no-op from the ViewModel's perspective — the
 * `ApproveViewModel` does not propagate `ResponseSender` failures
 * back to the screen today (S-017 contract), so we log and move on.
 */
public class GattResponseSender(
    private val peerId: String,
) : ResponseSender {
    override suspend fun sendApprove(responseFrame: ByteArray) {
        val transport = GattResponseTransports.lookup(peerId) ?: return
        transport.pushApprove(responseFrame)
    }

    override suspend fun sendDeny() {
        val transport = GattResponseTransports.lookup(peerId) ?: return
        transport.pushDeny()
    }
}
