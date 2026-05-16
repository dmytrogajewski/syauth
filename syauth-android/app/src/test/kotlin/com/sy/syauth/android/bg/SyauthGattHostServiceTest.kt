// syauth — Robolectric tests for the v0.1 demo host service.
//
// The service is started in the test under Robolectric's
// `buildService` harness. The test installs a fake controller factory
// and a fake intent dispatcher; it then drives a synthetic challenge
// through the controller's `pendingChallengeCallback` and asserts the
// dispatcher saw an intent with the right extras + that the service
// installed bond-key / hostname seams on the existing companion-
// service registry.
package com.sy.syauth.android.bg

import android.content.Intent
import android.util.Base64
import com.sy.syauth.android.provision.BOND_RECORD_FILE_NAME
import com.sy.syauth.android.provision.BondStore
import com.sy.syauth.android.provision.PROVISION_KEY_BYTES
import com.sy.syauth.android.provision.PROVISION_SCHEMA_VERSION
import com.sy.syauth.android.provision.ProvisionPackage
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

private const val TEST_PEER_ID: String = "abcdef0123456789abcdef0123456789"
private const val TEST_HOST_NAME: String = "test-desktop"
private const val TEST_CREATED_AT: String = "2026-05-17T10:00:00Z"

private val FIXTURE_BOND_KEY: ByteArray = ByteArray(PROVISION_KEY_BYTES) { it.toByte() }
private val FIXTURE_SEED: ByteArray = ByteArray(PROVISION_KEY_BYTES) { (0x40 + it).toByte() }
private val FIXTURE_PUBKEY: ByteArray = ByteArray(PROVISION_KEY_BYTES) { (0x80 + it).toByte() }

private fun seedBondStore() {
    val store = BondStore(RuntimeEnvironment.getApplication().filesDir)
    val pkg = ProvisionPackage(
        schemaVersion = PROVISION_SCHEMA_VERSION,
        hostName = TEST_HOST_NAME,
        peerId = TEST_PEER_ID,
        bondKey = FIXTURE_BOND_KEY,
        phoneSigningKeySeed = FIXTURE_SEED,
        phonePubkey = FIXTURE_PUBKEY,
        createdAt = TEST_CREATED_AT,
    )
    store.save(pkg)
}

private class RecordingController : GattServerController {
    var started: Int = 0
    var stopped: Int = 0
    var lastCallback: ((String, ByteArray) -> Unit)? = null
    override fun start(
        association: android.companion.AssociationInfo?,
        onChallenge: (peerId: String, frameBytes: ByteArray) -> Unit,
    ) {
        started += 1
        lastCallback = onChallenge
    }
    override fun stop() {
        stopped += 1
    }
}

private class RecordingIntentDispatcher : ApproveIntentDispatcher {
    var lastIntent: Intent? = null
    var dispatches: Int = 0
    override fun dispatch(context: android.content.Context, intent: Intent) {
        dispatches += 1
        lastIntent = intent
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class SyauthGattHostServiceTest {

    private lateinit var controller: RecordingController
    private lateinit var dispatcher: RecordingIntentDispatcher

    @Before
    fun setUp() {
        SyauthGattHostService.resetSeams()
        SyauthCompanionService.resetSeams()
        GattResponseTransports.reset()
        // Wipe any pre-existing bond record so each test starts fresh.
        java.io.File(RuntimeEnvironment.getApplication().filesDir, BOND_RECORD_FILE_NAME).delete()

        controller = RecordingController()
        dispatcher = RecordingIntentDispatcher()
        // Install fakes BEFORE buildService — onCreate consults them.
        SyauthGattHostService.controllerFactory = HostControllerFactory { _, _ -> controller }
        SyauthGattHostService.approveIntentDispatcher = dispatcher
        // Install a fake challenge verifier so the test path does not
        // attempt to load the UniFFI native library. The verifier
        // accepts everything and returns the frame bytes as the
        // "payload" so the host service treats the frame as valid.
        SyauthCompanionService.challengeVerifier = ChallengeVerifier { _, frame -> frame }
    }

    @After
    fun tearDown() {
        SyauthGattHostService.resetSeams()
        SyauthCompanionService.resetSeams()
        GattResponseTransports.reset()
    }

    @Test
    fun onCreate_starts_controller_when_bond_present() {
        seedBondStore()
        Robolectric.buildService(SyauthGattHostService::class.java).create()
        assertEquals(1, controller.started)
        assertNotNull(controller.lastCallback)
    }

    @Test
    fun onCreate_stops_self_when_no_bond_present() {
        // Do NOT seed bond — onCreate must stopSelf.
        Robolectric.buildService(SyauthGattHostService::class.java).create()
        assertEquals(0, controller.started)
    }

    @Test
    fun onCreate_installs_bond_key_provider_seam_keyed_by_peer_id() {
        seedBondStore()
        Robolectric.buildService(SyauthGattHostService::class.java).create()
        val provider = SyauthCompanionService.bondKeyProvider
        assertNotNull(provider)
        assertArrayEquals(FIXTURE_BOND_KEY, provider!!.bondKeyFor(TEST_PEER_ID))
        assertNull(provider.bondKeyFor("unknown-peer"))
    }

    @Test
    fun onCreate_installs_hostname_resolver_seam() {
        seedBondStore()
        Robolectric.buildService(SyauthGattHostService::class.java).create()
        val resolver = SyauthCompanionService.hostnameResolver
        assertNotNull(resolver)
        assertEquals(TEST_HOST_NAME, resolver!!.hostnameFor(TEST_PEER_ID))
    }

    @Test
    fun challenge_callback_dispatches_intent_to_main_activity() {
        seedBondStore()
        Robolectric.buildService(SyauthGattHostService::class.java).create()
        val cb = controller.lastCallback
        assertNotNull(cb)
        val frame = byteArrayOf(0x01, 0x02, 0x03, 0x04)

        cb!!.invoke(TEST_PEER_ID, frame)

        assertEquals(1, dispatcher.dispatches)
        val intent = dispatcher.lastIntent
        assertNotNull(intent)
        assertEquals(Intent.ACTION_VIEW, intent!!.action)
        assertEquals(TEST_HOST_NAME, intent.getStringExtra(EXTRA_HOSTNAME))
        assertEquals(TEST_PEER_ID, intent.getStringExtra(EXTRA_PEER_ID))
        val b64 = intent.getStringExtra(EXTRA_CHALLENGE_B64)
        assertNotNull(b64)
        val decoded = Base64.decode(b64, B64_FLAGS)
        assertArrayEquals(frame, decoded)
    }

    @Test
    fun challenge_callback_drops_frame_when_verifier_rejects() {
        seedBondStore()
        // Override the verifier to reject everything.
        SyauthCompanionService.challengeVerifier = ChallengeVerifier { _, _ -> null }
        Robolectric.buildService(SyauthGattHostService::class.java).create()
        val cb = controller.lastCallback
        assertNotNull(cb)
        cb!!.invoke(TEST_PEER_ID, byteArrayOf(9, 9, 9))
        assertEquals(0, dispatcher.dispatches)
    }

    @Test
    fun registers_gatt_response_transport_for_peer() {
        seedBondStore()
        Robolectric.buildService(SyauthGattHostService::class.java).create()
        val transport = GattResponseTransports.lookup(TEST_PEER_ID)
        assertNotNull(transport)
    }

    @Test
    fun onDestroy_stops_controller_and_unregisters_transport() {
        seedBondStore()
        val controllerScenario =
            Robolectric.buildService(SyauthGattHostService::class.java).create()
        val transportBefore = GattResponseTransports.lookup(TEST_PEER_ID)
        assertNotNull(transportBefore)

        controllerScenario.destroy()

        assertEquals(1, controller.stopped)
        val transportAfter = GattResponseTransports.lookup(TEST_PEER_ID)
        assertNull(transportAfter)
    }

    @Test
    fun controllerFactory_seam_is_consulted_by_onCreate() {
        seedBondStore()
        val custom = RecordingController()
        SyauthGattHostService.controllerFactory = HostControllerFactory { _, _ -> custom }
        Robolectric.buildService(SyauthGattHostService::class.java).create()
        assertEquals(1, custom.started)
        // The default factory was NOT used because we replaced the seam.
        assertTrue(controller.started == 0)
        assertSame(custom, custom)
    }
}
