// syauth — top-level Compose theme.
//
// Wraps Material 3 `MaterialTheme` with the Sy-prefixed dark color
// scheme + typography. Also configures the system status / navigation
// bars to render dark (white icons) so the activity edges blend with
// the [SyBlack] background.
//
// Mirrors prrr-android's `PrrrVPNTheme` one-to-one (same `SideEffect`,
// same status/nav bar wiring) so the two sibling apps share visual
// identity end to end.
package com.sy.syauth.android.ui.theme

import android.app.Activity
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.SideEffect
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.platform.LocalView
import androidx.core.view.WindowCompat

/**
 * Material 3 dark color scheme used app-wide. Values are pinned to
 * [Color.kt] so a future colour-token change has one place to land.
 */
private val SyColorScheme = darkColorScheme(
    primary = SyWhite,
    onPrimary = SyBlack,
    secondary = SyTextDim,
    onSecondary = SyBlack,
    tertiary = SySuccess,
    background = SyBlack,
    onBackground = SyWhite,
    surface = SySurface,
    onSurface = SyWhite,
    surfaceVariant = SySurface,
    onSurfaceVariant = SyTextDim,
    outline = SyBorder,
    error = SyDanger,
    onError = SyWhite,
    errorContainer = SyDanger.copy(alpha = 0.15f),
    onErrorContainer = SyDanger,
)

/**
 * Apply the syauth visual identity to [content]. Replaces the bare
 * `MaterialTheme {}` MainActivity used during the S-015..S-018
 * implementation push. Drives:
 *
 * * the Compose color scheme (via [SyColorScheme]);
 * * the system status + navigation bars (via [WindowCompat]) so the
 *   activity edges blend with [SyBlack];
 * * the typography scale (via [SyTypography]).
 */
@Composable
public fun SyauthTheme(content: @Composable () -> Unit) {
    val view = LocalView.current
    if (!view.isInEditMode) {
        SideEffect {
            val window = (view.context as Activity).window
            window.statusBarColor = SyBlack.toArgb()
            window.navigationBarColor = SyBlack.toArgb()
            WindowCompat.getInsetsController(window, view).isAppearanceLightStatusBars = false
            WindowCompat.getInsetsController(window, view).isAppearanceLightNavigationBars = false
        }
    }
    MaterialTheme(
        colorScheme = SyColorScheme,
        typography = SyTypography,
        content = content,
    )
}
