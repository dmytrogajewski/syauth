// Roadmap item S-016 — BT-bond removal seam.
//
// DoD #3: "On `Failed`, the bond is not persisted on either side. The
// Bluetooth bond is also removed (`BluetoothDevice.removeBond()` via
// reflection — Android does not expose this in the public SDK; document
// the reflection)."
//
// `BluetoothDevice.removeBond()` has been a hidden API since API 1 and is
// still hidden in API 34. The production impl lives in
// `pair.impl.ReflectionBondRemover` and is the ONLY place reflection on
// this method exists. The screen never calls `removeBond()` directly —
// always through this interface — so tests can inject a fake whose call
// count is asserted.
package com.sy.syauth.android.pair.api

/**
 * Removes the platform Bluetooth bond for a peer.
 *
 * Production wiring uses reflection on
 * `android.bluetooth.BluetoothDevice#removeBond(): Boolean`. The contract
 * here is intentionally untyped (no `BluetoothDevice` reference) so the
 * JVM unit-test classpath does not have to load the Android Bluetooth
 * stack. The production impl resolves the [PeerId] to a `BluetoothDevice`
 * internally.
 */
fun interface BluetoothBondRemover {
    /**
     * Remove the BT bond for the peer identified by [peerId].
     *
     * Returns `true` if removal succeeded, `false` on any reflection or
     * platform failure. The ViewModel ignores the return value (the BT
     * cleanup is best-effort; the *app-level* bond non-persistence is
     * what matters for our security model) but logs it for forensics.
     */
    fun remove(peerId: String): Boolean
}
