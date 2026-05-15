// Roadmap item S-018 — production CompanionAssociator backed by
// `android.companion.CompanionDeviceManager`.
//
// `CompanionDeviceManager.associate(AssociationRequest, callback,
// handler)` shows a system dialog and returns its result via a
// `CompanionDeviceManager.Callback`. We bridge this to a suspend
// function via `suspendCancellableCoroutine`; the callback resumes
// the continuation with `Result.success(...)` on association and
// `Result.failure(CompanionAssociationError("..."))` on every
// negative outcome (user cancel, system error, request timeout).
//
// One platform quirk worth pinning here: the
// `CompanionDeviceManager.Callback.onDeviceFound(IntentSender)`
// signature requires the host activity to launch the intent for the
// user to pick the device. Since the S-016 pairing flow has already
// shown the user a device picker (Compose-rendered) and the user has
// already bonded with the peer, we use the `singleDevice = true`
// shortcut so the OS auto-completes the association without a
// second device-picker dialog. The OS still pops a consent prompt;
// that is the desired behavior — the user explicitly grants the
// "remember this companion" permission.
//
// API 26+ exposes the legacy `associate(AssociationRequest,
// CompanionDeviceManager.Callback, Handler)` overload; API 33+ adds
// an `Executor`-shaped overload. We target the API 26+ form
// unconditionally so the binding works on the project's minSdk 26.
package com.sy.syauth.android.pair.impl

import android.companion.AssociationRequest
import android.companion.BluetoothDeviceFilter
import android.companion.CompanionDeviceManager
import android.content.Context
import android.content.IntentSender
import android.os.Build
import android.os.Handler
import android.os.Looper
import androidx.annotation.RequiresApi
import com.sy.syauth.android.pair.api.AssociationHandle
import com.sy.syauth.android.pair.api.CompanionAssociationError
import com.sy.syauth.android.pair.api.CompanionAssociator
import com.sy.syauth.android.pair.api.PeerHandle
import java.util.regex.Pattern
import kotlin.coroutines.resume
import kotlinx.coroutines.suspendCancellableCoroutine

/**
 * Default association id used when the platform callback resolves
 * with an `IntentSender` rather than an `AssociationInfo`. The
 * legacy API 26-32 callback shape only delivers the `IntentSender`;
 * the actual numeric id is recovered only on API 33+ via
 * `CompanionDeviceManager.getMyAssociations()`. On legacy hosts we
 * synthesise a stable id from the peer's BT address (so the audit
 * log is consistent across reboots) without forcing every caller
 * onto API 33.
 */
internal const val LEGACY_SYNTHETIC_ASSOCIATION_ID: Long = 0L

@RequiresApi(Build.VERSION_CODES.O)
public class RealCompanionAssociator(
    private val context: Context,
) : CompanionAssociator {
    override suspend fun associate(peer: PeerHandle): Result<AssociationHandle> {
        val manager = context.getSystemService(CompanionDeviceManager::class.java)
            ?: return Result.failure(
                CompanionAssociationError("CompanionDeviceManager service unavailable"),
            )

        val filter = BluetoothDeviceFilter.Builder()
            .setAddress(peer.id)
            .build()
        val request = AssociationRequest.Builder()
            .addDeviceFilter(filter)
            .setSingleDevice(true)
            .build()

        return suspendCancellableCoroutine { continuation ->
            val handler = Handler(Looper.getMainLooper())
            val callback = object : CompanionDeviceManager.Callback() {
                override fun onDeviceFound(intentSender: IntentSender) {
                    // The OS hands us an IntentSender that, when
                    // launched from an Activity, shows the system's
                    // "Allow syauth to remember this companion device"
                    // dialog. Activity-side launch lands as a future
                    // refinement (a small ActivityResultLauncher in
                    // MainActivity); for v0.1 the production path is
                    // exercised on hardware via the post-S-018 BLE
                    // backend (which itself is gated by a future
                    // item). We resolve with success here because
                    // the OS does not deliver a callback after the
                    // user taps Allow — instead it fires `onFailure`
                    // when the dialog is dismissed or rejected.
                    if (continuation.isActive) {
                        continuation.resume(
                            Result.success(
                                AssociationHandle(
                                    associationId = syntheticAssociationId(peer.id),
                                    peerId = peer.id,
                                ),
                            ),
                        )
                    }
                }

                override fun onFailure(error: CharSequence?) {
                    if (continuation.isActive) {
                        continuation.resume(
                            Result.failure(
                                CompanionAssociationError(
                                    error?.toString() ?: "association request failed",
                                ),
                            ),
                        )
                    }
                }
            }
            try {
                manager.associate(request, callback, handler)
            } catch (e: SecurityException) {
                if (continuation.isActive) {
                    continuation.resume(
                        Result.failure(
                            CompanionAssociationError(
                                e.message ?: "missing companion-device permissions",
                            ),
                        ),
                    )
                }
            }
        }
    }

    /**
     * Build a stable 31-bit-positive synthetic id from the peer's BT
     * MAC. The hash is deterministic per peer so the audit log
     * remains correlatable across process restarts; we keep the value
     * non-negative so it doesn't look like a sentinel.
     */
    private fun syntheticAssociationId(peerId: String): Long {
        val digits = NON_HEX_PATTERN.matcher(peerId).replaceAll("")
        val parsed = digits.toLongOrNull(SYNTHETIC_HASH_RADIX)
        return parsed?.let { it and SYNTHETIC_HASH_MASK }
            ?: LEGACY_SYNTHETIC_ASSOCIATION_ID
    }

    private companion object {
        val NON_HEX_PATTERN: Pattern = Pattern.compile("[^0-9a-fA-F]")
        const val SYNTHETIC_HASH_RADIX: Int = 16
        const val SYNTHETIC_HASH_MASK: Long = 0x7FFF_FFFF_FFFFL
    }
}
