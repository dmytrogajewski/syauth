// syauth — typography scale.
//
// Mirrors prrr-android's typography so headings / body / labels align
// between the two apps. Only `bodyLarge` is overridden; everything
// else falls through to Material 3 defaults.
package com.sy.syauth.android.ui.theme

import androidx.compose.material3.Typography
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp

/** Material 3 typography scale used by [SyauthTheme]. */
public val SyTypography: Typography = Typography(
    bodyLarge = TextStyle(
        fontFamily = FontFamily.Default,
        fontWeight = FontWeight.Normal,
        fontSize = 16.sp,
        lineHeight = 24.sp,
        letterSpacing = 0.5.sp,
    ),
)
