// Roadmap item S-017 — Approve screen Compose UI.
//
// Renders the user-facing surface for every unlock. Layout:
//
//   - App icon (Material lock icon) at the top.
//   - Hostname header — "Approve unlock for <hostname>?"
//   - Two buttons side-by-side (Approve / Deny). Approve is the
//     primary; Deny is the destructive variant.
//   - Countdown line — "Approve within Xs" (rendered only while the
//     state is `Counting`).
//   - Terminal status line — "Unlock approved." or "Denied: <reason>"
//     while the state is `Approved` or `Denied`.
//
// The screen is a pure function of `ApproveUiState`. All side-effects
// (start the countdown, dispatch Approve, dispatch Deny) flow through
// the `ApproveViewModel` injected as a parameter.
package com.sy.syauth.android.approve

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTag
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle

/**
 * Test tags attached to the major nodes so the androidTest harness can
 * find them without depending on the rendered strings (which are
 * locale- and config-dependent in future iterations).
 */
public object ApproveScreenTestTags {
    public const val HOSTNAME: String = "syauth.approve.hostname"
    public const val APPROVE_BUTTON: String = "syauth.approve.approve_button"
    public const val DENY_BUTTON: String = "syauth.approve.deny_button"
    public const val COUNTDOWN: String = "syauth.approve.countdown"
    public const val TERMINAL: String = "syauth.approve.terminal"
}

/** Maximum hostname length rendered. Excess is ellipsized. */
private const val HOSTNAME_MAX_CHARS: Int = 64

/** Screen padding. */
private val SCREEN_PADDING_DP = 24.dp

/** Vertical spacing between major sections. */
private val SECTION_SPACING_DP = 16.dp

/** App-icon size at the top of the screen. */
private val APP_ICON_SIZE_DP = 56.dp

/** Horizontal spacing between the Approve and Deny buttons. */
private val BUTTON_SPACING_DP = 12.dp

/**
 * Render the Approve screen for [viewModel]. The composable is a pure
 * function of `viewModel.uiState`; every action flows back through
 * `viewModel.onApproveClicked` / `viewModel.onDenyClicked`.
 */
@Composable
public fun ApproveScreen(viewModel: ApproveViewModel) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()

    // Start the countdown exactly once when the screen enters
    // composition. `LaunchedEffect(Unit)` keys to the screen lifecycle.
    LaunchedEffect(Unit) {
        viewModel.start()
    }

    val safeHostname = sanitizeHostname(viewModel.hostname)
    val approveEnabled = state is ApproveUiState.Counting

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(SCREEN_PADDING_DP),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Top,
    ) {
        Icon(
            imageVector = Icons.Filled.Lock,
            contentDescription = null,
            modifier = Modifier.size(APP_ICON_SIZE_DP),
            tint = MaterialTheme.colorScheme.primary,
        )
        Spacer(modifier = Modifier.height(SECTION_SPACING_DP))
        Text(
            text = "Approve unlock for $safeHostname?",
            style = MaterialTheme.typography.headlineSmall,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.semantics { testTag = ApproveScreenTestTags.HOSTNAME },
        )
        Spacer(modifier = Modifier.height(SECTION_SPACING_DP))
        CountdownRow(state)
        Spacer(modifier = Modifier.height(SECTION_SPACING_DP))
        ButtonRow(
            approveEnabled = approveEnabled,
            onApprove = viewModel::onApproveClicked,
            onDeny = viewModel::onDenyClicked,
        )
        Spacer(modifier = Modifier.height(SECTION_SPACING_DP))
        TerminalMessage(state)
    }
}

@Composable
private fun CountdownRow(state: ApproveUiState) {
    val text = when (state) {
        is ApproveUiState.Counting -> "Approve within ${state.remainingSeconds}s"
        ApproveUiState.AwaitingBiometric -> "Awaiting biometric…"
        ApproveUiState.Signing -> "Signing…"
        else -> ""
    }
    Text(
        text = text,
        style = MaterialTheme.typography.bodyLarge,
        modifier = Modifier.semantics { testTag = ApproveScreenTestTags.COUNTDOWN },
    )
}

@Composable
private fun ButtonRow(
    approveEnabled: Boolean,
    onApprove: () -> Unit,
    onDeny: () -> Unit,
) {
    Row(
        horizontalArrangement = Arrangement.Center,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Button(
            onClick = onApprove,
            enabled = approveEnabled,
            modifier = Modifier.semantics { testTag = ApproveScreenTestTags.APPROVE_BUTTON },
        ) {
            Text("Approve")
        }
        Spacer(modifier = Modifier.width(BUTTON_SPACING_DP))
        OutlinedButton(
            onClick = onDeny,
            enabled = approveEnabled,
            colors = ButtonDefaults.outlinedButtonColors(
                contentColor = MaterialTheme.colorScheme.error,
            ),
            modifier = Modifier.semantics { testTag = ApproveScreenTestTags.DENY_BUTTON },
        ) {
            Text("Deny")
        }
    }
}

@Composable
private fun TerminalMessage(state: ApproveUiState) {
    val text = when (state) {
        is ApproveUiState.Approved -> "Unlock approved."
        is ApproveUiState.Denied -> "Denied: ${denialReasonLabel(state.reason)}"
        else -> ""
    }
    Text(
        text = text,
        style = MaterialTheme.typography.bodyMedium,
        modifier = Modifier.semantics { testTag = ApproveScreenTestTags.TERMINAL },
    )
}

private fun denialReasonLabel(reason: DenialReason): String = when (reason) {
    DenialReason.UserDenied -> "user denied"
    DenialReason.TimedOut -> "timed out"
    DenialReason.BiometricFailed -> "biometric failed"
    DenialReason.BiometricUnavailable -> "biometric unavailable"
    is DenialReason.SignError -> "sign error (${reason.reason})"
}

/**
 * Cap the hostname at [HOSTNAME_MAX_CHARS] characters and strip
 * newlines so an attacker-controlled name cannot inject a multi-line
 * phishing prompt. Defends T-014 (biometric coercion / phishing).
 */
internal fun sanitizeHostname(raw: String): String {
    val singleLine = raw.replace('\n', ' ').replace('\r', ' ')
    return if (singleLine.length > HOSTNAME_MAX_CHARS) {
        singleLine.substring(0, HOSTNAME_MAX_CHARS) + "…"
    } else {
        singleLine
    }
}
