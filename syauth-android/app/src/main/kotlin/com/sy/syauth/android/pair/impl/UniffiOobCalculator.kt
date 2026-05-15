// Roadmap item S-016 — production OOB calculator.
//
// DoD #2: "OOB code is computed via the UniFFI surface
// (`oobCodeForBond`) — never reimplemented in Kotlin." This is the entire
// production wiring. It is a one-liner so the reviewer can confirm at a
// glance that no Kotlin HKDF re-implementation exists.
//
// Byte-identity with the desktop CLI's OOB output is pinned by
// `oob_byte_identical_to_cli_fixture` in
// `crates/syauth-mobile/src/implementation.rs`. If the Rust side ever
// drifts, that Rust test fails first, well before any Kotlin caller can
// notice.
package com.sy.syauth.android.pair.impl

import com.sy.syauth.android.pair.api.OobCalculator
import uniffi.syauth_mobile.oobCodeForBond

/**
 * Production [OobCalculator] backed by the UniFFI-generated
 * `oobCodeForBond` binding. NEVER reimplement the HKDF in Kotlin.
 */
class UniffiOobCalculator : OobCalculator {
    override fun compute(bondKey: ByteArray): List<String> =
        oobCodeForBond(bondKey)
}
