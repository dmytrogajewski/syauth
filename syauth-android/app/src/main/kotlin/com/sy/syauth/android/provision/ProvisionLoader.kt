// syauth — provision-file loader.
//
// First-launch bootstrap reads the desktop-emitted
// `syauth-provision.toml` from the app's external private files
// directory. The operator pushes the file there with
//
//     adb push syauth-provision.toml \
//         /sdcard/Android/data/com.sy.syauth.android/files/syauth-provision.toml
//
// The loader is intentionally small — it is exercised exactly once
// per install, and the file is deleted immediately after the
// BondStore has persisted the bond.
//
// External-private (`Context.getExternalFilesDir(null)`) is the
// right pick on Android 14+: it is world-writable by adb without
// the app needing `MANAGE_EXTERNAL_STORAGE`, yet the app reads it
// without any permission grant. The legacy `/sdcard/Download/`
// location requires `READ_MEDIA_*` permissions that do not cover
// `.toml` files at all.
package com.sy.syauth.android.provision

import android.content.Context
import java.io.File

/** File name the desktop CLI emits. Mirrored from `crates/syauth-cli`. */
public const val PROVISION_FILE_NAME: String = "syauth-provision.toml"

/**
 * Resolve the canonical path of the provision file inside the app's
 * external-private files dir. Exposed so callers (and the bootstrap
 * routine) can both lookup and delete the same file.
 */
public fun provisionFilePath(context: Context): File {
    val dir = context.getExternalFilesDir(null) ?: context.filesDir
    return File(dir, PROVISION_FILE_NAME)
}

/**
 * Load + parse the provision file from the app's external-private
 * dir if present. Returns `null` when no provision file has been
 * pushed yet.
 */
@Throws(ProvisionParseError::class)
public fun loadProvisionFromDownloads(context: Context): ProvisionPackage? {
    val target = provisionFilePath(context)
    if (!target.exists()) return null
    return parseProvisionPackage(target.readText())
}
