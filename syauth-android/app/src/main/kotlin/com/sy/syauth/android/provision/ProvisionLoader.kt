// syauth — provision-file loader.
//
// First-launch bootstrap reads the desktop-emitted
// `syauth-provision.toml` from the phone's public `Downloads/`
// directory (where `adb push <file> /sdcard/Download/` lands). The
// loader is intentionally small — it is exercised exactly once per
// install, and the file is deleted immediately after the BondStore
// has persisted the bond.
//
// The Downloads location is by design: it is the canonical adb-push
// landing pad on modern Android (the legacy `/sdcard/` is no longer
// app-readable on API 30+ without `MANAGE_EXTERNAL_STORAGE`). The
// public `getExternalStoragePublicDirectory(DIRECTORY_DOWNLOADS)`
// path remains readable by ordinary apps for files the user (or
// `adb`) placed there themselves.
package com.sy.syauth.android.provision

import android.os.Environment
import java.io.File

/** File name the desktop CLI emits. Mirrored from `crates/syauth-cli`. */
public const val PROVISION_FILE_NAME: String = "syauth-provision.toml"

/**
 * Resolve the canonical path of the provision file inside the
 * phone's public Downloads directory. Exposed so callers (and the
 * bootstrap routine) can both lookup and delete the same file.
 */
public fun provisionFilePath(): File =
    File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS), PROVISION_FILE_NAME)

/** Load + parse the provision file from Downloads if present. */
@Throws(ProvisionParseError::class)
public fun loadProvisionFromDownloads(): ProvisionPackage? {
    val target = provisionFilePath()
    if (!target.exists()) return null
    return parseProvisionPackage(target.readText())
}
