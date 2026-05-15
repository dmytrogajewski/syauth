// Roadmap item S-016 — Pairing Compose screen.
//
// The screen is a pure projection of [PairingState] to Compose nodes. It
// never reads side-effect state, never owns timers, never calls into
// `BluetoothDevice.removeBond()` (DoD #3 — the screen MUST go through
// the [BluetoothBondRemover] interface via the ViewModel).
//
// Every interactive node carries a `testTag` so the Compose UI test in
// `PairingScreenTest.kt` can target it without depending on rendered
// text. The test tags are the public contract of the screen for tests.
//
// Why a `state: PairingState` parameter instead of a `viewModel: …`:
//   The Compose UI test renders this screen against fixed states; the
//   ViewModel is exercised by the Robolectric unit tests. Passing the
//   state in lets us test each branch independently without spinning
//   up the entire ViewModel + fakes for a render check.
package com.sy.syauth.android.pair

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTag
import androidx.compose.ui.unit.dp

/**
 * Test tags. Public so [PairingScreenTest] can use the same constants.
 */
object PairingTestTags {
    const val IDLE_CTA: String = "pair.idle.cta"
    const val SCANNING_PROGRESS: String = "pair.scanning.progress"
    const val SCANNING_CANCEL: String = "pair.scanning.cancel"
    const val LESC_CODE: String = "pair.lesc.code"
    const val LESC_CANCEL: String = "pair.lesc.cancel"
    const val OOB_WORDS: String = "pair.oob.words"
    const val OOB_YES: String = "pair.oob.yes"
    const val OOB_NO: String = "pair.oob.no"
    const val BONDED_LABEL: String = "pair.bonded.label"
    const val BONDED_DONE: String = "pair.bonded.done"
    const val FAILED_REASON: String = "pair.failed.reason"
    const val FAILED_BACK: String = "pair.failed.back"
}

/** Static UI strings. Centralised so tests assert on the same constants. */
object PairingStrings {
    const val IDLE_CTA: String = "Pair with computer"
    const val CANCEL: String = "Cancel"
    const val OOB_QUESTION: String = "These match the computer?"
    const val OOB_YES: String = "Yes"
    const val OOB_NO: String = "No"
    const val BONDED_PREFIX: String = "Paired with "
    const val DONE: String = "Done"
    const val FAILED_PREFIX: String = "Pairing failed: "
    const val BACK: String = "Back"
}

/**
 * Pairing screen.
 *
 * @param state authoritative state from [PairingViewModel.state].
 * @param onStartScan invoked when the Idle CTA is tapped.
 * @param onCancel invoked from Scanning / LescNegotiating.
 * @param onOobYes invoked from OobConfirming.
 * @param onOobNo invoked from OobConfirming.
 * @param onDone invoked from Bonded / Failed — pops the route back to home.
 */
@Composable
fun PairingScreen(
    state: PairingState,
    onStartScan: () -> Unit,
    onCancel: () -> Unit,
    onOobYes: () -> Unit,
    onOobNo: () -> Unit,
    onDone: () -> Unit,
) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(24.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        when (state) {
            is PairingState.Idle -> IdleContent(onStartScan = onStartScan)
            is PairingState.Scanning -> ScanningContent(onCancel = onCancel)
            is PairingState.LescNegotiating -> LescContent(
                code = state.code,
                onCancel = onCancel,
            )
            is PairingState.OobConfirming -> OobContent(
                emoji = state.emoji,
                onYes = onOobYes,
                onNo = onOobNo,
            )
            is PairingState.Bonded -> BondedContent(name = state.name, onDone = onDone)
            is PairingState.Failed -> FailedContent(reason = state.reason, onBack = onDone)
        }
    }
}

@Composable
private fun IdleContent(onStartScan: () -> Unit) {
    Button(
        onClick = onStartScan,
        modifier = Modifier
            .fillMaxWidth()
            .semantics { testTag = PairingTestTags.IDLE_CTA },
    ) {
        Text(text = PairingStrings.IDLE_CTA)
    }
}

@Composable
private fun ScanningContent(onCancel: () -> Unit) {
    CircularProgressIndicator(
        modifier = Modifier.semantics { testTag = PairingTestTags.SCANNING_PROGRESS },
    )
    Spacer(modifier = Modifier.height(24.dp))
    Button(
        onClick = onCancel,
        modifier = Modifier.semantics { testTag = PairingTestTags.SCANNING_CANCEL },
    ) {
        Text(text = PairingStrings.CANCEL)
    }
}

@Composable
private fun LescContent(code: String, onCancel: () -> Unit) {
    Text(
        text = code,
        style = MaterialTheme.typography.headlineLarge,
        modifier = Modifier.semantics { testTag = PairingTestTags.LESC_CODE },
    )
    Spacer(modifier = Modifier.height(24.dp))
    Button(
        onClick = onCancel,
        modifier = Modifier.semantics { testTag = PairingTestTags.LESC_CANCEL },
    ) {
        Text(text = PairingStrings.CANCEL)
    }
}

@Composable
private fun OobContent(
    emoji: List<String>,
    onYes: () -> Unit,
    onNo: () -> Unit,
) {
    Text(
        text = emoji.joinToString(separator = " "),
        style = MaterialTheme.typography.headlineMedium,
        modifier = Modifier.semantics { testTag = PairingTestTags.OOB_WORDS },
    )
    Spacer(modifier = Modifier.height(16.dp))
    Text(text = PairingStrings.OOB_QUESTION)
    Spacer(modifier = Modifier.height(16.dp))
    Row(horizontalArrangement = Arrangement.spacedBy(16.dp)) {
        Button(
            onClick = onYes,
            modifier = Modifier.semantics { testTag = PairingTestTags.OOB_YES },
        ) {
            Text(text = PairingStrings.OOB_YES)
        }
        Button(
            onClick = onNo,
            modifier = Modifier.semantics { testTag = PairingTestTags.OOB_NO },
        ) {
            Text(text = PairingStrings.OOB_NO)
        }
    }
}

@Composable
private fun BondedContent(name: String, onDone: () -> Unit) {
    Text(
        text = PairingStrings.BONDED_PREFIX + name,
        modifier = Modifier.semantics { testTag = PairingTestTags.BONDED_LABEL },
    )
    Spacer(modifier = Modifier.height(24.dp))
    Button(
        onClick = onDone,
        modifier = Modifier.semantics { testTag = PairingTestTags.BONDED_DONE },
    ) {
        Text(text = PairingStrings.DONE)
    }
}

@Composable
private fun FailedContent(reason: String, onBack: () -> Unit) {
    Text(
        text = PairingStrings.FAILED_PREFIX + reason,
        modifier = Modifier.semantics { testTag = PairingTestTags.FAILED_REASON },
    )
    Spacer(modifier = Modifier.height(24.dp))
    Button(
        onClick = onBack,
        modifier = Modifier.semantics { testTag = PairingTestTags.FAILED_BACK },
    ) {
        Text(text = PairingStrings.BACK)
    }
}
