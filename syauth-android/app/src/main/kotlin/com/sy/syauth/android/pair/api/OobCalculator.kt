// Roadmap item S-016 — OOB-code computation seam.
//
// DoD #2: "OOB code is computed via the UniFFI surface (`oobCodeForBond`)
// — never reimplemented in Kotlin." The production [OobCalculator] impl
// (`UniffiOobCalculator` in `pair.impl`) delegates to
// `uniffi.syauth_mobile.oobCodeForBond(bondKey)`. The interface lives in
// the `api` subpackage so unit tests can inject a fake without dragging
// the UniFFI/JNA load chain into the JVM classpath.
package com.sy.syauth.android.pair.api

/**
 * Computes the 4-word emoji OOB code from a bond key.
 *
 * The contract is byte-identical to the desktop CLI's output (SPEC §4.1
 * "Why a second OOB confirmation after BT pairing"). The byte-identity
 * is pinned by `oob_byte_identical_to_cli_fixture` in
 * `crates/syauth-mobile/src/implementation.rs`.
 *
 * Production wiring (S-018) feeds the negotiated bond key into this
 * calculator; tests feed fixed bytes to deterministic fakes.
 */
fun interface OobCalculator {
    /**
     * Compute the OOB code for [bondKey]. Implementations MAY throw if
     * the bond key length is wrong, but the ViewModel only ever passes
     * lengths produced by [PairBackend], so a length error is a wiring
     * bug, not a user error.
     *
     * Returns exactly four words (the syauth-mobile OOB contract).
     */
    fun compute(bondKey: ByteArray): List<String>
}
