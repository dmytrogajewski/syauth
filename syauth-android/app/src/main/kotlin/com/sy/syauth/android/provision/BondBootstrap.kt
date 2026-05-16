// syauth — first-launch bond bootstrap.
//
// On every cold-start, MainActivity invokes [bootstrapBond] to resolve
// the bond record. The routine prefers the on-disk store (so a relaunch
// stays bonded without the provision file lingering); falls back to
// the Downloads provision file on first launch; and on a successful
// fallback, DELETES the source file so the plaintext Ed25519 seed does
// not linger in shared storage.
//
// The function is idempotent: a second call after a successful
// bootstrap simply re-reads the persisted store.
//
// Failures are surfaced as `null` (with a logcat warning) rather than
// thrown. The caller has no recovery path: if the bootstrap fails,
// the user must adb-push a fresh provision file and relaunch.
package com.sy.syauth.android.provision

import android.content.Context
import android.util.Log

/** Logcat tag used by every bootstrap span. Pinned for log greppability. */
public const val BOOTSTRAP_LOG_TAG: String = "syauth.bootstrap"

/** Message surfaced as a toast when no bond is available on first launch. */
public const val BOOTSTRAP_NO_BOND_TOAST: String =
    "No syauth bond — adb push syauth-provision.toml to the app's external-private dir and relaunch"

/**
 * Resolve the bond record. Returns `null` when neither the on-disk
 * store nor the provision file is present (or when either parse
 * fails — the routine logs and treats parse failure as "no bond" so
 * a corrupt file never crashes the activity).
 */
public fun bootstrapBond(context: Context): BondRecord? {
    val store = BondStore(context.filesDir)
    val existing = runCatching { store.load() }
        .onFailure { Log.w(BOOTSTRAP_LOG_TAG, "bond store load failed: ${it.message}") }
        .getOrNull()
    if (existing != null) {
        Log.i(BOOTSTRAP_LOG_TAG, "bond loaded from on-disk store")
        return existing
    }
    val provisioned = runCatching { loadProvisionFromDownloads(context) }
        .onFailure {
            Log.w(BOOTSTRAP_LOG_TAG, "provision file present but parse failed: ${it.message}")
        }
        .getOrNull()
    if (provisioned == null) {
        Log.i(BOOTSTRAP_LOG_TAG, "no provision file pushed; bootstrap is null")
        return null
    }
    val saved = runCatching { store.save(provisioned) }
    if (saved.isFailure) {
        Log.w(
            BOOTSTRAP_LOG_TAG,
            "could not persist bond from provision: ${saved.exceptionOrNull()?.message}",
        )
        return null
    }
    // Remove the source file ONLY after the persisted copy is on disk.
    // If the delete fails we still proceed — the bond is usable;
    // surfacing a warning is enough.
    val source = provisionFilePath(context)
    if (source.exists() && !source.delete()) {
        Log.w(BOOTSTRAP_LOG_TAG, "could not delete consumed provision file: $source")
    }
    Log.i(BOOTSTRAP_LOG_TAG, "bond bootstrapped from provision file and persisted")
    return BondRecord(
        peerId = provisioned.peerId,
        hostName = provisioned.hostName,
        bondKey = provisioned.bondKey,
        phoneSigningKeySeed = provisioned.phoneSigningKeySeed,
        phonePubkey = provisioned.phonePubkey,
    )
}
