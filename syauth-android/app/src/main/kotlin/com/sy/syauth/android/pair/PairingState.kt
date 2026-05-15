// Roadmap item S-016 — pairing-screen state machine.
//
// Mirrors `~/sources/prrr/prrr-android/app/src/main/kotlin/com/prrr/vpn/android/
// scanner/ScanState.kt` in shape: a `sealed class` with a singleton `Idle`,
// transient progress variants, and terminal `Bonded` / `Failed` payloads.
//
// The state machine is the Android side of the same flow that desktop
// `syauth-cli pair` runs (S-011). It is the canonical pairing workflow from
// SPEC §4.4. The Compose screen and the ViewModel both consume this state;
// no other state shape is allowed to leak into either of them.
//
// Why a `sealed class` and not an enum + payload bag:
//   - Each variant carries a different payload shape (a 6-digit code, a
//     4-element word list, a peer name, a failure reason). An enum would
//     force a single payload class, which would either be sparse (most
//     fields null per variant) or untyped (a `Map<String, Any>`). Both are
//     bug-magnets — the SPEC §4.4 note is explicit: "Mixing them is the
//     most common bug class in similar projects."
//   - The Kotlin compiler's exhaustiveness check on `when (state)` flags a
//     forgotten variant at every call-site. That is the property we depend
//     on for the Compose screen to never silently drop a state.
package com.sy.syauth.android.pair

/**
 * Authoritative state for the pairing screen.
 *
 * Transitions are driven exclusively by [PairingViewModel]; the
 * [PairingScreen] is a pure projection of this value to Compose nodes.
 *
 * Variant contracts:
 * - [Idle]: nothing has happened yet. The screen renders the "Pair with
 *   computer" CTA. The ViewModel exits this state only via
 *   `onStartScanTapped()`.
 * - [Scanning]: scanning is in progress. The screen renders a progress
 *   indicator and a cancel button.
 * - [LescNegotiating]: the BT LE Secure Connections bond is in flight. The
 *   `code` is the 6-digit numeric-comparison code that the OS pairing
 *   dialog is also displaying; we render it big so the user can compare it
 *   against the desktop CLI's output (SPEC §4.1 Pair step 3).
 * - [OobConfirming]: BT pairing succeeded and we have computed the
 *   app-level OOB via UniFFI's `oobCodeForBond`. `emoji` is exactly four
 *   words (pinned by `OOB_WORD_COUNT = 4` in syauth-mobile).
 * - [Bonded]: terminal success state. `name` is the peer's friendly name
 *   for display.
 * - [Failed]: terminal failure state. `reason` is human-readable and
 *   actionable; it never contains key material or secret bytes (SPEC §6
 *   T-010, mirroring the syauth-mobile MobileError contract).
 */
sealed class PairingState {

    /** Initial state. The CTA is the only interactive element. */
    data object Idle : PairingState()

    /** Scanning is in flight; the user may cancel back to [Idle]. */
    data object Scanning : PairingState()

    /**
     * BT LE Secure Connections is in flight; `code` is the 6-digit
     * numeric-comparison code displayed by the OS pairing dialog.
     *
     * The code is informational on our side — the OS owns the actual
     * comparison; we render it so the user has two surfaces showing the
     * same code (defense in depth against a fake system dialog).
     */
    data class LescNegotiating(val code: String) : PairingState()

    /**
     * BT pairing succeeded; the app-level 4-word OOB code is ready.
     *
     * `emoji` is the four-element list returned by
     * `uniffi.syauth_mobile.oobCodeForBond(bondKey)`. We display it; the
     * user compares it against the desktop CLI's display and taps Yes/No.
     */
    data class OobConfirming(val emoji: List<String>) : PairingState()

    /**
     * Terminal success. `name` is the peer's display name; the screen
     * renders "Paired with $name" plus a "Done" button that routes home.
     */
    data class Bonded(val name: String) : PairingState()

    /**
     * Terminal failure. `reason` is a single-line, actionable string
     * (e.g. "adapter FakeAdapter-4.0 does not support LE Secure
     * Connections", "OOB code did not match — peer might be a relay
     * attacker"). It never contains secret bytes.
     *
     * Entering [Failed] is the cleanup signal for the
     * `BluetoothBondRemover`: the ViewModel removes the BT bond before
     * emitting this state, so the screen reading [Failed] can be sure no
     * bond is left over on either side (DoD #3).
     */
    data class Failed(val reason: String) : PairingState()
}
