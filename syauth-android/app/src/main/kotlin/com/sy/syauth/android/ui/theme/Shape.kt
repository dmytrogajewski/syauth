// syauth — Material 3 Shapes scale.
//
// Pinned to RoundedCornerShape(8.dp) across the small/medium/large
// tokens so every Material 3 component (Button, OutlinedButton, Card,
// AlertDialog) renders with the same near-square corner radius as the
// prrr-android sibling app's per-call shape (PrrrVPNTheme uses 8.dp
// directly on the Connect button; we hoist that to the theme so
// individual screens never need a `shape =` override).
package com.sy.syauth.android.ui.theme

import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Shapes
import androidx.compose.ui.unit.dp

/** Global corner radius shared by every component in the SyauthTheme. */
internal val SyCornerRadius = 8.dp

/**
 * Material 3 shape scale. All five tokens get the same radius so the
 * operator never sees a stray pill-rounded button next to a square
 * one. Matches prrr-android's `RoundedCornerShape(8.dp)` per-button
 * style.
 */
public val SyShapes: Shapes = Shapes(
    extraSmall = RoundedCornerShape(SyCornerRadius),
    small = RoundedCornerShape(SyCornerRadius),
    medium = RoundedCornerShape(SyCornerRadius),
    large = RoundedCornerShape(SyCornerRadius),
    extraLarge = RoundedCornerShape(SyCornerRadius),
)
