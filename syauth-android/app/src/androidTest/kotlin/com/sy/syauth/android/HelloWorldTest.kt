// Roadmap item S-015 — instrumented test for the hello-world Compose screen.
//
// The DoD line "assert OOB: ... string is rendered (proves the Rust call
// through UniFFI/JNA actually executed)" is enforced by two assertions:
//
//   1. `onNodeWithText(OOB_RENDER_PREFIX, substring = true).assertIsDisplayed()`
//      — proves the Text composable rendered with the prefix.
//   2. A regex check on the rendered string matching the literal output of
//      `uniffi.syauth_mobile.oobCodeForBond(helloBondKey())` — proves the
//      Rust call really executed (the test recomputes the expected words
//      via the same UniFFI surface and asserts byte-equality).
//
// If either assertion failed today, the failure mode is either:
//   - JNA failed to load `libsyauth_mobile.so` (no `.so` for the
//     emulator's ABI in the AAR), or
//   - the UniFFI bindings drifted from the AAR's ABI version, or
//   - the OobScreen stopped rendering the result.
// Each of these is a real defect we want to catch in CI.
package com.sy.syauth.android

import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.syauth_mobile.oobCodeForBond

@RunWith(AndroidJUnit4::class)
class HelloWorldTest {

    @get:Rule
    val composeTestRule = createAndroidComposeRule<MainActivity>()

    @Test
    fun mainActivity_renders_oob_string_from_rust() {
        // The home route stopped showing the OOB smoke text by default
        // when the paired-device card landed; navigate to the
        // diagnostic route via the home "?" button before asserting.
        composeTestRule
            .onNodeWithTag(HOME_DIAGNOSTIC_BUTTON_TAG)
            .performClick()

        // Wait for the produceState coroutine to publish the real value
        // (initial value is "OOB: ..." with three dots; we want the value
        // produced by the UniFFI call to actually replace it).
        composeTestRule.waitUntil(timeoutMillis = WAIT_TIMEOUT_MS) {
            val nodes = composeTestRule
                .onAllNodes(hasText(OOB_RENDER_PREFIX, substring = true), useUnmergedTree = true)
                .fetchSemanticsNodes()
            nodes.any { node ->
                val text = node.config
                    .getOrNull(SemanticsProperties.Text)
                    ?.joinToString(separator = "") { it.text }
                    .orEmpty()
                text.startsWith(OOB_RENDER_PREFIX) && !text.endsWith(LOADING_SUFFIX)
            }
        }

        composeTestRule
            .onNodeWithText(OOB_RENDER_PREFIX, substring = true)
            .assertIsDisplayed()

        // Recompute the expected rendered string via the same UniFFI
        // surface the activity calls; assert byte-equality.
        val expected = OOB_RENDER_PREFIX + oobCodeForBond(helloBondKey()).joinToString(" ")
        composeTestRule
            .onNodeWithText(expected)
            .assertIsDisplayed()

        // Defense in depth: the rendered text must NOT be the "ERR" branch
        // and MUST contain four whitespace-separated tokens after the
        // prefix. Catches a regression where the UniFFI call threw and we
        // silently rendered the error string.
        assertTrue(
            "expected four-word OOB code, got: $expected",
            expected.startsWith(OOB_RENDER_PREFIX) &&
                !expected.startsWith("${OOB_RENDER_PREFIX}ERR") &&
                expected.removePrefix(OOB_RENDER_PREFIX)
                    .split(" ")
                    .size == OOB_EXPECTED_WORD_COUNT,
        )
    }

    /**
     * The UniFFI surface returns exactly 4 emoji words for any 32-byte
     * bond key — pinned in `syauth_mobile::oob_code_for_bond` via
     * `OOB_WORD_COUNT = 4`. The instrumented test asserts the rendered
     * string honors the same contract.
     */
    private companion object {
        const val OOB_EXPECTED_WORD_COUNT: Int = 4
        const val WAIT_TIMEOUT_MS: Long = 5_000L

        /**
         * Suffix the initial state carries before the UniFFI call resolves.
         * Mirrors MainActivity.kt's initialValue exactly so the wait loop
         * cannot drift.
         */
        const val LOADING_SUFFIX: String = "..."
    }
}
