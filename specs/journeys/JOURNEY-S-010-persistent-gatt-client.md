# JOURNEY-S-010: Phone `PersistentGattClient` with `autoConnect=true`

> **Spec anchors:** `specs/unlock-proximity/SPEC.md` §3 Approach
> ("**`SyauthCompanionService`** (Android) — becomes a long-running
> foreground service ... Maintains a single `BluetoothGatt` client per
> bonded peer, opened with `autoConnect=true` and `TRANSPORT_LE`.
> Subscribes to the challenge characteristic via CCCD write on every
> fresh service discovery.").
>
> §3 Decisions row "Phone connection lifecycle" — "One persistent
> `BluetoothGatt` per bonded peer, opened with `autoConnect=true` and
> held by `SyauthCompanionService` as a long-running foreground
> service".
>
> §4 Architecture diagram — phone-side stack lists the
> `PersistentGattClient` (`autoConnect=true`) box subscribing via
> CCCD and routing `onCharacteristicChanged` into the verifier.
>
> **Roadmap row:** `specs/unlock-proximity/ROADMAP.md` Step S-010.
>
> **Closure condition (verbatim from ROADMAP.md):**
>
> ```
> ./gradlew :app:testDebugUnitTest --tests "*PersistentGattClientTest*"
> ```

## Roadmap Link
- Source roadmap: [specs/unlock-proximity/ROADMAP.md](../unlock-proximity/ROADMAP.md) Step S-010.
- Feature: a new phone-side class
  `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/PersistentGattClient.kt`
  that owns one `BluetoothGatt` per bonded peer, opened with
  `BluetoothDevice.connectGatt(context, autoConnect=true, callback,
  TRANSPORT_LE)`. On `onServicesDiscovered` it enables notifications
  on the challenge characteristic via
  `setCharacteristicNotification(challenge, true)` plus a CCCD write
  of `CCCD_ENABLE_NOTIFY`; on `onCharacteristicChanged` it forwards
  the bytes through an `onChallenge(peerId, frameBytes)` callback;
  exposes `writeResponse(frameBytes)` so the Approve flow can write
  the signed bytes back on the response characteristic. The file is
  introduced as a sibling to the existing 154-line
  `DirectGattController.kt` (which uses `autoConnect=false` and the
  CDM proximity-binding); S-011 swaps the service to use this new
  client, S-013 deletes `DirectGattController.kt`. S-010 does NOT
  modify the service or the controller — it only adds the new file
  and its Robolectric tests.

## 1. Journey

When **an Android user has paired their phone with the desktop
(BondRecord persisted, MAC known) and the phone-side foreground
service is running**, I want to **hold a persistent BLE GATT
connection to the desktop with `autoConnect=true` so the OS
transparently re-establishes the link across out-of-range / sleep
transitions, notifications on the challenge characteristic stay
subscribed across reconnections, and every challenge frame is
delivered to the verifier within one BLE round-trip**, so I can
**unlock my desktop via the `sudo` prompt with sub-2-second
end-to-end latency without the foreground service ever having to
re-scan, re-pair, or re-discover services on every prompt**.

## 2. CJM

Before S-010 the phone-side BLE path is `DirectGattController` —
opened with `autoConnect=false`, gated on a `CompanionDeviceManager`
proximity-binding callback that the OS schedules at battery-saver
duty cycle. Every desktop `sudo` waits for the next CDM scan
(seconds to minutes), then opens a fresh GATT (200–500 ms BLE
connect), discovers services, writes the CCCD, then receives the
challenge — well over the SPEC §4.3 "< 2.0 s" budget. S-010 inverts
that: the GATT connection is up at idle, the CCCD subscription
survives reconnect (the Android stack re-applies the CCCD on
service-rediscovery), and every challenge is a single notify
round-trip on an already-open link.

### Phase 1: Fresh first connect after pairing

**User Intent:** The user has just paired the phone with the
desktop, the foreground service has just been started, and the
phone needs to open the persistent GATT link for the first time.

**Actions:**
1. `SyauthCompanionService.onCreate` (wired in S-011) constructs one
   `PersistentGattClient` per `BondRecord` and calls `.start()`.
2. `PersistentGattClient.start()` resolves the desktop MAC via
   `BluetoothAdapter.getRemoteDevice(deviceMac)`, then calls
   `GattOpener.open(device, autoConnect = true, callback)` which
   delegates to `BluetoothDevice.connectGatt(context, true, callback,
   TRANSPORT_LE)`.
3. The OS BLE stack starts the directed-advertising listen and
   connects as soon as the desktop's advertisement is observed (the
   long-lived `syauth-presenced` advertiser from S-001..S-009 is up
   the moment the desktop session starts).
4. `onConnectionStateChange(STATE_CONNECTED)` fires; the client
   calls `g.discoverServices()`.
5. `onServicesDiscovered` fires; the client locates the challenge
   characteristic by `SYAUTH_CHALLENGE_CHAR_UUID`, calls
   `setCharacteristicNotification(challenge, true)`, then writes
   `CCCD_ENABLE_NOTIFY` to the CCCD descriptor.

**Pain / Risk:**
- Desktop advertiser is not yet up (e.g. `syauth-presenced.service`
  is in `activating`): `connectGatt` returns a handle but the link
  never establishes. `autoConnect=true` makes this self-healing —
  the OS keeps trying — but the test must assert the autoConnect
  flag is set so the resilience is real, not assumed.
- Challenge characteristic is missing from the discovered services
  (mis-paired desktop with stale UUID): `findCharacteristic` returns
  null, no CCCD write, and no `onChallenge` callback can ever fire.
  The DoD test pins the CCCD-write path so this regression surfaces
  in unit tests.
- The OS may invoke `connectGatt` overloads other than the 4-arg
  form (Android added `phy` and `handler` parameters in API 26
  and 28). We pin the 4-arg overload (`Context`, `autoConnect`,
  `callback`, `transport`) which is supported on the minSdk-26
  floor, and isolate the call behind a `GattOpener` seam.

**Success Signal:** The Robolectric test
`auto_connect_true_passed_to_connectGatt` observes the
`GattOpener.open(...)` call captured with `autoConnect = true`,
exactly once, with the expected `BluetoothDevice` and a non-null
`BluetoothGattCallback`.

### Phase 2: Phone goes out of range and OS reconnects without app intervention

**User Intent:** The user walks away from the desktop (out of BLE
range), then returns. The connection must reattach silently so the
next `sudo` does not pay reconnect latency at the application
layer.

**Actions:**
1. `onConnectionStateChange(STATE_DISCONNECTED)` fires when the
   peer goes out of range. The client does NOT call
   `gatt.close()` — `autoConnect=true` instructs the OS to retry.
2. The user walks back into range. The OS BLE stack reconnects on
   its own schedule (not subject to background-app throttling per
   the JavaDoc cited in SPEC §2 "Technical Context").
3. `onConnectionStateChange(STATE_CONNECTED)` fires again. The
   client calls `g.discoverServices()` again.
4. `onServicesDiscovered` re-runs the CCCD-subscribe path so notify
   delivery is restored. This is the same code path Phase 1
   exercises, but on a reused `BluetoothGatt` handle.

**Pain / Risk:**
- If the client closes the GATT handle on the first disconnect, the
  `autoConnect=true` promise is broken — the OS no longer holds the
  retry intent. `stop()` is the only entry that closes the handle
  (and clears the stored reference).
- If the CCCD write is dropped on the second `onServicesDiscovered`
  (e.g. the implementation memoises a "subscribed" flag), the
  desktop's notify fails silently. The DoD test
  `on_services_discovered_subscribes_via_cccd` pins the CCCD-write
  path so a future "optimisation" can't drop it.
- If `setCharacteristicNotification` returns `false` (Android stack
  refusing because no CCCD descriptor exists), there's no point
  attempting the descriptor write. The implementation guards both
  arms.

**Success Signal:** Robolectric's `ShadowBluetoothGatt` captures
the `setCharacteristicNotification(challenge, true)` call and the
CCCD descriptor's `value` matches `CCCD_ENABLE_NOTIFY` after the
shadow's `BluetoothGattCallback.onServicesDiscovered` is invoked.

### Phase 3: Challenge characteristic notify lands and the `onChallenge` callback fires

**User Intent:** The desktop issued a fresh `sudo`. The
`syauth-presenced` daemon NOTIFIED a challenge frame on the
challenge characteristic. The phone must deliver those bytes to
the verifier so the Approve flow can show the BiometricPrompt.

**Actions:**
1. The desktop writes a challenge frame; the BLE stack delivers it
   to the phone as `BluetoothGattCallback.onCharacteristicChanged`.
2. The client's callback inspects `characteristic.uuid`; if it
   matches `SYAUTH_CHALLENGE_CHAR_UUID`, the bytes are forwarded
   verbatim to `onChallenge(peerId, frameBytes)`.
3. Both the pre-API-33 (`onCharacteristicChanged(g,
   characteristic)`) and API-33+ (`onCharacteristicChanged(g,
   characteristic, value)`) overrides invoke the callback, so the
   client behaves correctly on every API level the manifest
   declares.

**Pain / Risk:**
- A notify lands for a characteristic other than the challenge
  (e.g. a future battery-level service): the UUID guard drops it
  silently. The DoD test pins this so the guard never disappears.
- A null `characteristic.value` on the pre-API-33 path: the
  pre-API-33 override reads `characteristic.value`; if null, the
  callback is suppressed. (The shadow's
  `writeIncomingCharacteristic` always sets a non-null value, so
  the test exercises the happy path.)
- The callback throws: the contract is "the caller MUST NOT
  throw". The DoD test asserts the callback is invoked exactly
  once per delivered frame; throwing-callback resilience is a
  future-step concern (S-011's verifier already swallows
  exceptions).

**Success Signal:** The Robolectric test
`on_characteristic_changed_invokes_onChallenge` injects a
`BluetoothGattCharacteristic` carrying a known byte payload, drives
`ShadowBluetoothGatt`'s notify path, and asserts the test's
`onChallenge` lambda was invoked once with the expected `peerId`
and a byte-for-byte equal payload.

### Phase 4: Approve flow writes the signed response back

**User Intent:** The user has approved the prompt; the Keystore
has produced a signed response frame. The phone must write those
bytes to the response characteristic so the desktop's PAM call
can complete.

**Actions:**
1. The Approve flow looks up the `PersistentGattClient` for the
   target peer (S-011 wires this) and calls
   `writeResponse(frameBytes)`.
2. The client finds the response characteristic on the cached
   `BluetoothGatt` services, sets its `value` to `frameBytes`, and
   calls `gatt.writeCharacteristic(c)`.
3. The method returns the boolean from `writeCharacteristic` (the
   caller can log a failure for diagnostic purposes).

**Pain / Risk:**
- The GATT handle is null (the client was stopped, or never
  started): `writeResponse` returns `false` without throwing. The
  Approve flow can surface a recoverable error to the user.
- The response characteristic is missing (mis-paired desktop):
  same — `writeResponse` returns `false`.
- Race between `writeResponse` and `stop()`: stop() atomically
  swaps the handle to null; the write either runs against a live
  handle or returns false. No use-after-close.

**Success Signal:** The Robolectric test
`write_response_targets_response_characteristic` injects a service
containing both characteristics, calls `writeResponse(payload)`,
and asserts the shadow GATT's "last written bytes" equal `payload`
exactly.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|---|---|---|
| Robolectric's `ShadowBluetoothDevice` does not expose a `getAutoConnect()` getter on `connectGatt(...)`, so the test cannot assert the autoConnect flag without a seam. | 1 | Introduce a `GattOpener` interface; production binds it to `device.connectGatt(...)`; the test supplies a fake that records every argument. The seam survives S-013's delete-DirectGattController step. |
| The pre-API-33 `onCharacteristicChanged(g, c)` and the API-33+ `onCharacteristicChanged(g, c, v)` overrides must both route into the same `onChallenge` callback. | 3 | Copy the dual-override pattern from `DirectGattController.kt` lines 128–140 directly into `PersistentGattClient` — but duplicate the constants (`CCCD_UUID`, `CCCD_ENABLE_NOTIFY`) instead of importing, because S-013 deletes the file we'd import from. |
| `stop()` must be idempotent so accidental double-stop from S-011's `onDestroy` + watchdog interplay is safe. | 2 | Use `AtomicReference<BluetoothGatt?>.getAndSet(null)` — the second call sees null and is a no-op. |

### North Star Summary

The persistent GATT client is the phone's anchor for sub-2-second
unlock latency. Opened once at service start, held at idle for the
operator's entire session, reattached by the OS across range
transitions without app code running, the client delivers every
desktop challenge as a single notify on an already-open link and
writes the signed response back on the same handle. With S-010 the
phone-side stack matches the canonical Android background-BLE
pattern (Apple Watch / CCC Digital Key / Tesla phone-as-key) and
the per-PAM-call BLE connect ritual of `DirectGattController` is
gone for good in S-013.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] First persistent connect lands within one BLE advertise
      interval of `start()` — bounded by the OS, not by the app.
- [x] Robolectric tests give a green-bar signal in under 5 s on a
      developer laptop.

### Onboarding Clarity
- [x] The class has a kdoc explaining the `autoConnect=true`
      decision and links the SPEC clause.
- [x] Failure paths (`stop`-after-`stop`, write-after-`stop`) log
      with the `syauth.bg.persistent` tag so field debugging via
      `adb logcat` is one grep away.

### Production-Ready Defaults
- [x] `autoConnect = true` is the only call site — there is no
      "demo" `autoConnect = false` path.
- [x] `TRANSPORT_LE` is pinned (not auto / BR-EDR).

### Golden Path Quality
- [x] Connect → discover → subscribe → notify → forward callback is
      exercised end-to-end by the Robolectric test that simulates
      `onCharacteristicChanged`.

### Decision Load
- [x] Constructor takes only what the class strictly needs
      (`context`, `adapter`, `peerId`, `deviceMac`, `onChallenge`).
- [x] No configuration toggles, no boolean params — the only knob
      is the test-injected `GattOpener`.

### Progressive Complexity
- [x] Production code calls `start()` once at service boot; tests
      exercise `start → notify → writeResponse → stop` in isolation
      without standing up the full service.

### Error Quality
- [x] Missing challenge characteristic logs `not present` at WARN
      and silently returns (no crash, no NPE).
- [x] `writeResponse` returns `false` on every recoverable error
      (no handle, no characteristic, stack refuses); the caller can
      surface a user-facing message.

### Failure Safety
- [x] `stop()` is idempotent — calling twice is a no-op.
- [x] `writeResponse` never throws.

### Runtime Transparency
- [x] Every state transition (`connecting`, `discovered`,
      `subscribed`, `frame received`, `stopped`) emits a structured
      logcat line under tag `syauth.bg.persistent`.

### Debuggability
- [x] The captured `peerId` is forwarded verbatim to `onChallenge`
      so logs in the service and logs here are joinable.

### Cross-Surface Consistency
- [x] Characteristic UUID constants match
      `crates/syauth-transport/src/bluez.rs` (the desktop side) and
      `GattServer.kt` (the existing phone-side declarations).

### Workflow Consistency
- [x] The test file lives in
      `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/`
      (the existing convention used by `BleScanControllerTest.kt`
      and `ApproveNotificationTest.kt`).
- [x] The Robolectric runner + `@Config(sdk = [34])` annotation
      matches the existing test pattern.

### Change Safety
- [x] The new file is additive — no existing file is modified by
      S-010.
- [x] S-011's swap of the service is a separate, smaller diff.

### Experimentation Safety
- [x] The test seam (`GattOpener`) is package-private; production
      consumers cannot inject a fake by accident.

### Interaction Latency
- [x] No `Thread.sleep`, no polling — every callback is event-driven.

### Developer Feedback Speed
- [x] `:app:testDebugUnitTest --tests "*PersistentGattClientTest*"`
      runs in seconds; no instrumented-test rig needed.

### Team Scale
- [x] The file is reviewable on its own (≤ 200 lines) and the test
      file pins every contract.

### System Scale
- [x] One instance per bonded peer — `ConcurrentHashMap<peerId,
      PersistentGattClient>` in S-011 scales to multi-peer.

### Right Behavior by Default
- [x] `autoConnect = true` is the production wiring; no fallback
      to `false`.

### Anti-Bypass Design
- [x] The `GattOpener` interface has exactly one production impl;
      a future contributor cannot accidentally pass `false` because
      `start()` doesn't take an `autoConnect` parameter.

## 4. Tests

### TC-01: `auto_connect_true_passed_to_connectGatt`

**Given** a Robolectric-driven `PersistentGattClient` constructed
with a fake `GattOpener` that records every `open(...)` argument.
**When** the test calls `client.start()`.
**Then** the recorded `autoConnect` flag is `true`, the recorded
device's MAC matches the constructor's `deviceMac`, and the
recorded callback is non-null. Exactly one `open(...)` invocation
is recorded.

### TC-02: `on_services_discovered_subscribes_via_cccd`

**Given** a `PersistentGattClient` whose `GattOpener` returns a
shadow `BluetoothGatt` with a discovered service that has both the
challenge characteristic (with a CCCD descriptor) and the response
characteristic.
**When** the test drives `ShadowBluetoothGatt`'s
`onConnectionStateChange(STATE_CONNECTED)` then
`onServicesDiscovered`.
**Then** `ShadowBluetoothGatt` reports that
`setCharacteristicNotification(challenge, true)` was called and the
CCCD descriptor's `value` equals `CCCD_ENABLE_NOTIFY`.

### TC-03: `on_characteristic_changed_invokes_onChallenge`

**Given** a fully-started `PersistentGattClient` with a subscribed
challenge characteristic and a recorder lambda for `onChallenge`.
**When** the test invokes
`BluetoothGattCallback.onCharacteristicChanged(g, challenge,
payload)` with the challenge UUID and a known payload.
**Then** the recorder was called exactly once with the constructor's
`peerId` and a byte-for-byte equal payload. A second notify on a
different UUID does NOT invoke the recorder.

### TC-04: `write_response_targets_response_characteristic`

**Given** a fully-started `PersistentGattClient` and a payload
`response_bytes`.
**When** `client.writeResponse(response_bytes)` is called.
**Then** the shadow GATT reports the response characteristic was
the target of the last `writeCharacteristic`, its `value` equals
`response_bytes`, and the method returned `true`.

## Acceptance Criteria

- [x] `PersistentGattClient.kt` exists with the contract above.
- [x] `PersistentGattClientTest::auto_connect_true_passed_to_connectGatt`
      passes.
- [x] `PersistentGattClientTest::on_services_discovered_subscribes_via_cccd`
      passes.
- [x] `PersistentGattClientTest::on_characteristic_changed_invokes_onChallenge`
      passes.
- [x] `PersistentGattClientTest::write_response_targets_response_characteristic`
      passes.
- [x] `:app:assembleDebug` succeeds.
- [x] `:app:testDebugUnitTest` green.

## Traceability
- Roadmap item: `specs/unlock-proximity/ROADMAP.md` Step S-010.
- Implementation files: filled in the Implementation section below.
- Test files: filled in the Implementation section below.

## Implementation

Files created in S-010:
- `syauth-android/app/src/main/kotlin/com/sy/syauth/android/bg/PersistentGattClient.kt`
  — new sibling to `DirectGattController.kt`; owns one
  `BluetoothGatt` per bonded peer opened with `autoConnect=true`;
  exposes `start()`, `stop()`, `writeResponse(frameBytes)`;
  package-private `GattOpener` seam captures `autoConnect` in
  tests because Robolectric's `ShadowBluetoothDevice` 4.11.1 lacks
  a `getAutoConnect()` getter.
- `syauth-android/app/src/test/kotlin/com/sy/syauth/android/bg/PersistentGattClientTest.kt`
  — Robolectric-driven JVM test that pins all four DoD test cases:
  `auto_connect_true_passed_to_connectGatt`,
  `on_services_discovered_subscribes_via_cccd`,
  `on_characteristic_changed_invokes_onChallenge`,
  `write_response_targets_response_characteristic`.

Files modified in S-010: none. The file is wired into
`SyauthCompanionService` in S-011; `DirectGattController.kt` is
deleted in S-013.

Key seams introduced:
- `GattOpener` — package-private functional interface
  (`fun open(device: BluetoothDevice, autoConnect: Boolean,
  callback: BluetoothGattCallback): BluetoothGatt?`). Production
  binds `DefaultGattOpener` which calls
  `device.connectGatt(context, autoConnect, callback,
  TRANSPORT_LE)`. Tests inject a recording fake.

Constants (duplicated, not imported, per the scope brief — see
"Files likely affected" / S-013 plans to delete
`DirectGattController.kt`):
- `CCCD_UUID = UUID.fromString("00002902-0000-1000-8000-00805f9b34fb")`
- `CCCD_ENABLE_NOTIFY = byteArrayOf(0x01, 0x00)`

Characteristic UUIDs are imported from `GattServer.kt` because
that file is the single source of truth for the syauth wire UUIDs
(`SYAUTH_CHALLENGE_CHAR_UUID`, `SYAUTH_RESPONSE_CHAR_UUID`) and is
not slated for deletion.
