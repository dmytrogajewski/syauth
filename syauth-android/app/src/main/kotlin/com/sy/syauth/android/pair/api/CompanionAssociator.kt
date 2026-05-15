// Roadmap item S-018 — CompanionDeviceManager association seam.
//
// At pair-complete (S-016 OobConfirming -> Bonded happy path), the
// PairingViewModel must register the bonded peer with
// `CompanionDeviceManager.associate()` so the OS will bind our
// CompanionDeviceService (S-018, see `bg/SyauthCompanionService.kt`)
// whenever the peer comes back into BLE range. Without that
// association, the OS never wakes the app.
//
// The seam is a one-method suspend interface so the test can inject a
// fake that resolves synchronously to either `Result.success(handle)` or
// `Result.failure(...)`. Production wiring lives in
// `pair/impl/RealCompanionAssociator.kt` and calls into the
// `android.companion.CompanionDeviceManager` Java API.
//
// We intentionally avoid leaking the framework `AssociationInfo` class
// across this interface — that class has a hidden constructor and is
// nightmare to fabricate in unit tests on a JVM-only host. The
// `AssociationHandle` data class carries the only field the
// ViewModel actually cares about (the opaque association id); the real
// `AssociationInfo` is still consumed inside the production impl and
// inside the service.
package com.sy.syauth.android.pair.api

/**
 * Opaque handle returned by a successful association. Carries only the
 * fields the ViewModel and the audit log need; deliberately does not
 * expose the framework `AssociationInfo` so the JVM unit test surface
 * stays free of Android-companion-framework imports.
 *
 * @property associationId the OS-assigned numeric id; stable for the
 *   lifetime of the association.
 * @property peerId the same `PeerHandle.id` the user just bonded with.
 */
public data class AssociationHandle(
    val associationId: Long,
    val peerId: String,
)

/**
 * Typed error surface for [CompanionAssociator]. The reason is
 * user-presentable (no key material, no internal IDs).
 */
public class CompanionAssociationError(
    public val reason: String,
) : Exception(reason)

/**
 * Contract for requesting a CDM association. Production calls into
 * `CompanionDeviceManager.associate()`; tests inject a fake.
 *
 * `suspend` because the production call shows a user-visible dialog and
 * resolves on a callback.
 */
public fun interface CompanionAssociator {
    /**
     * Request an association for [peer]. Returns the handle on success
     * or a typed [CompanionAssociationError] wrapped in a failed
     * `Result`. Implementations MUST NOT throw.
     */
    public suspend fun associate(peer: PeerHandle): Result<AssociationHandle>
}
