// Roadmap item S-017 — response dispatcher seam.
//
// The Approve flow's terminal action is to ship either a
// `PeerApproved` frame (containing the Ed25519 signature) or a
// `PeerDenied` frame back to the desktop. The transport itself lives
// in S-018 (`CompanionDeviceService` + GATT server); S-017 talks to
// the transport via this minimal interface so the ViewModel's tests
// can record dispatch calls without standing up a fake GATT stack.
package com.sy.syauth.android.approve

/**
 * Contract for shipping the Approve flow's terminal frame back to the
 * desktop. Both methods are `suspend` because the underlying GATT
 * write is async; tests inject a fake that records the call and
 * returns immediately.
 */
public interface ResponseSender {
    /**
     * Ship a `PeerApproved` frame carrying [responseFrame] (the
     * 64-byte Ed25519 signature blob returned by UniFFI
     * `signChallengeResponse`).
     */
    public suspend fun sendApprove(responseFrame: ByteArray)

    /**
     * Ship a `PeerDenied` frame. The desktop interprets every
     * `PeerDenied` identically — there is no protocol-level
     * distinction between an explicit-user-deny and a phone-side
     * timeout, by design (defends T-014 biometric coercion: the
     * attacker cannot tell whether the user denied or just walked
     * away).
     */
    public suspend fun sendDeny()
}
