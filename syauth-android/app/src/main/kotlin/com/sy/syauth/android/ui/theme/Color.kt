// syauth — color palette.
//
// Mirrors prrr-android's dark palette (black surface, white text,
// dimmed grey secondary, green success accent, red danger) so the
// two sibling apps look like they ship from the same operator.
// Values are the exact hex codes prrr-android uses so a tooling
// audit (diff-the-palettes) catches drift.
package com.sy.syauth.android.ui.theme

import androidx.compose.ui.graphics.Color

/** Background base. Pure black so OLED panels save power. */
public val SyBlack: Color = Color(0xFF000000)

/** Card / surface background. One shade lighter than [SyBlack]. */
public val SySurface: Color = Color(0xFF111111)

/** Borders and dividers. */
public val SyBorder: Color = Color(0xFF333333)

/** Primary on-surface text. */
public val SyWhite: Color = Color(0xFFFFFFFF)

/** Secondary / hint text. */
public val SyTextDim: Color = Color(0xFFA0A0A0)

/** Approve-success accent. */
public val SySuccess: Color = Color(0xFF22C55E)

/** Warning accent (countdown low / retry needed). */
public val SyWarning: Color = Color(0xFFF59E0B)

/** Deny / error accent. */
public val SyDanger: Color = Color(0xFFEF4444)
