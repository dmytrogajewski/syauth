// DEV-003 inverted the BLE role pair mandated by SPEC §3.2 D8: the
// **desktop** advertises a rotating session-bound UUID; the **phone**
// scans and connects.
//
// Before DEV-003 this file held a phone-side GATT server controller
// plus a BLE advertiser shim. Both are gone — the phone no longer
// advertises and no longer hosts a GATT service. What remains is:
//
//   1. The `GattServerController` fun-interface, kept as the per-
//      association abstraction `SyauthCompanionService` consults on
//      every `onDeviceAppeared`. The interface name predates the
//      role inversion; the semantics now describe a controller that
//      runs the phone-side BLE flow for one associated peer
//      (regardless of whether the underlying transport is server-
//      hosted, client-driven, or both).
//
//   2. The challenge / response characteristic UUIDs the desktop
//      registers on its GATT application. The phone consumes them
//      as a CLIENT — subscribing to the challenge characteristic
//      for notify-pushed challenge frames and writing the response
//      bytes back on the response characteristic.
//
// The rotating service UUID is computed per-call via the UniFFI
// `sessionUuidForBond(bondKey, minuteBe)` surface; the static
// `SYAUTH_GATT_SERVICE_UUID` from the legacy code is gone (the
// service UUID is no longer fixed — that was the presence-tracking
// defect DEV-003 closes).
package com.sy.syauth.android.bg

import android.companion.AssociationInfo
import java.util.UUID

/**
 * Characteristic the desktop NOTIFIES the phone on with the challenge
 * frame. The phone subscribes via CCCD write and receives one frame
 * per challenge.
 */
public val SYAUTH_CHALLENGE_CHAR_UUID: UUID =
    UUID.fromString("5a4e8e3c-1c4c-4a17-9c81-d518a55a0002")

/**
 * Characteristic the phone WRITES the signed response onto. The
 * desktop's `bluer` peripheral reads the bytes via the
 * characteristic-control event stream.
 */
public val SYAUTH_RESPONSE_CHAR_UUID: UUID =
    UUID.fromString("5a4e8e3c-1c4c-4a17-9c81-d518a55a0003")

/**
 * Contract the [SyauthCompanionService] uses to drive the BLE flow
 * for one associated peer.
 *
 * `start(association, onChallenge)` is called from
 * `onDeviceAppeared`; the implementation begins its scan / connect /
 * subscribe sequence and routes every received challenge frame
 * through `onChallenge(peerId, frameBytes)`. The implementation MUST
 * be idempotent: calling `start` twice back-to-back is a no-op.
 *
 * `stop` is called from `onDeviceDisappeared`; it tears down the
 * scanner + any open GATT client connection. Idempotent — calling
 * `stop` on a stopped controller is a no-op.
 */
public interface GattServerController {
    /**
     * Begin the phone-side BLE flow for [association]. Every incoming
     * challenge frame from the bonded desktop is delivered as
     * `onChallenge(peerId, frameBytes)`; the desktop's bond peer id
     * is passed verbatim.
     *
     * `association` MAY be `null` when the controller is exercised in
     * a unit test that does not need to materialise an
     * `AssociationInfo`.
     */
    public fun start(
        association: AssociationInfo?,
        onChallenge: (peerId: String, frameBytes: ByteArray) -> Unit,
    )

    /** Tear down the scanner + any open GATT client connection. */
    public fun stop()
}
