---
name: bt
description: Bluetooth pairing and unlock-channel design, mocking, and e2e testing for syauth
---

# Agent Instructions: Bluetooth Unlock Channel Workflow

<constraints>
Do not run git commands. All version control is handled by the user.
Follow the persona and contracts defined in AGENTS.md.
Run `make lint` before considering any step complete.
Never test against the user's daily-driver phone or paired devices. Use an emulator, a dedicated test device, or a mock BlueZ peer.
Never call `unsafe` raw HCI ioctls when a `zbus` / BlueZ wrapper exists — see /ffi.
</constraints>

<role>
You are a Linux Bluetooth engineer focused on the syauth unlock channel between a desktop PAM module and an Android companion device. You know BlueZ's DBus surface (`org.bluez.Adapter1`, `Device1`, `GattCharacteristic1`), the difference between BR/EDR pairing and BLE bonding, and the implications of `Just Works` vs. numeric-comparison association models for an auth flow.
</role>

You design the pairing and unlock protocol so that **proximity is necessary but not sufficient**: every unlock must prove possession of a bonded device key, with a fresh challenge, in a bounded time window.

---

## When To Use This Skill

Invoke `/bt` when:
- Designing or changing the pairing flow.
- Changing the unlock packet format, characteristic UUID, or MTU.
- Debugging "device found but unlock fails" symptoms.
- Adding a new peer-state transition.
- Reviewing reliability/regression of the BT layer under flaky-link conditions.

For threat-model analysis of the resulting protocol (relay, MitM, replay), follow up with `/threat`.

---

## Phase 1: Pin The Protocol Surface

Document the exact wire-level contract before writing code. A vague protocol becomes an exploit.

For each BT operation, fill in:

```
Direction:       desktop → phone | phone → desktop
Transport:       BLE GATT write | notify | BR/EDR L2CAP CoC
Service UUID:    <128-bit>
Char UUID:       <128-bit>
MTU:             <bytes, after negotiation>
Frame format:    [version:1][nonce:16][payload:N][tag:16]
Auth:            HMAC-SHA256 over (version || nonce || payload) with bond_key
Timing budget:   request → response within 800ms; total unlock < 2s
Replay window:   nonce stored in a sliding 64-entry cache, evicted by LRU+TTL=10s
```

<rule>
Every frame carries (a) a version byte, (b) a fresh nonce, (c) an authenticator over the full frame. Frames without all three are dropped before parsing. No exceptions.
</rule>

---

## Phase 2: Pairing State Machine

Write the pairing flow as an explicit enum. Implicit state machines hide bypass bugs.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PairingState {
    Idle,
    Advertising { since: Instant },
    ProvisionalBonded { peer: PeerId, since: Instant }, // waiting for user confirmation
    Bonded { peer: PeerId },
    Revoked { peer: PeerId, reason: RevokeReason },
}
```

Required transitions, each gated by an explicit guard:

| From → To | Trigger | Guard |
|-----------|---------|-------|
| Idle → Advertising | user runs `syauth pair` | desktop is unlocked (current user is `root` or session is interactive) |
| Advertising → ProvisionalBonded | BLE bond + service-discovery completed | adapter UUID matches advertised UUID |
| ProvisionalBonded → Bonded | user confirms the numeric-comparison code on BOTH devices | timer ≤ 60s |
| ProvisionalBonded → Revoked | timer expired or codes mismatched | always |
| Bonded → Revoked | user runs `syauth revoke <peer>` OR ≥ N failed unlocks | configurable threshold |

<rule>
The unlock path NEVER reads from `ProvisionalBonded`. Only `Bonded` peers are queried during `pam_sm_authenticate`. Mixing the two states is the most common syauth bug class.
</rule>

---

## Phase 3: Mock The Peer

Real BT hardware is non-deterministic. Build the e2e tests against a mock peer that speaks the syauth protocol over the same DBus surface.

Recommended setup:
1. Run a `btvirt` (`bluez-tools`) virtual controller, OR run a userland `python-dbus` script that registers a fake `org.bluez.GattCharacteristic1` on the session bus.
2. Wrap BlueZ behind a trait so tests inject the mock:
   ```rust
   pub trait BtPeer: Send + Sync {
       fn write(&self, char_uuid: Uuid, data: &[u8]) -> Result<()>;
       fn subscribe(&self, char_uuid: Uuid) -> Result<Receiver<Vec<u8>>>;
       fn disconnect(&self) -> Result<()>;
   }
   ```
3. Production impl uses `zbus` against the system bus; test impl is an in-process channel.
4. Tests exercise: golden path, peer-offline, slow peer (>budget), reordered frames, replayed nonce, truncated frame, oversized frame, wrong-version frame.

<rule>
The BT trait boundary is the only place mocking is allowed in syauth. Do not mock crypto, do not mock the PAM handle. Mock once, at the radio.
</rule>

---

## Phase 4: Test Matrix

Every BT change must add or update at least one row in `tests/bt_matrix.rs`:

| Case | Setup | Expected unlock result | Expected log marker |
|------|-------|------------------------|---------------------|
| golden | peer online, valid bond | `PAM_SUCCESS` in < 2s | `bt.unlock.ok` |
| offline | peer unreachable | `PAM_AUTHINFO_UNAVAIL` after ≤ 1.2s | `bt.unlock.unreachable` |
| slow | peer responds at T+1.5s | `PAM_AUTHINFO_UNAVAIL` (over budget) | `bt.unlock.timeout` |
| replay | resend prior nonce | `PAM_AUTH_ERR` | `bt.unlock.nonce_reused` |
| wrong key | peer signs with stale bond | `PAM_AUTH_ERR` | `bt.unlock.bad_sig` |
| revoked | peer in Revoked state | `PAM_AUTH_ERR` (no radio attempt) | `bt.unlock.revoked` |
| downgrade | peer advertises older version | `PAM_AUTH_ERR` | `bt.unlock.version_rejected` |
| MTU split | frame straddles MTU | success, with two-segment reassembly | `bt.frame.reassembled` |

Add cases for every new state transition or frame field.

---

## Phase 5: Field Inspection

When something fails in the wild, capture in this order:

1. `bluetoothctl show` — adapter is powered, discoverable state, paired list.
2. `journalctl -t syauth -t bluetoothd --since "5 minutes ago"` — interleaved app/stack logs.
3. `sudo btmon -w /tmp/syauth.btsnoop` — full HCI capture; replayable in Wireshark with the `btsnoop` dissector.
4. On the phone: `adb logcat -s syauth-companion` if the Android companion app is reachable.

Attach btmon captures (sanitized of bond keys) to the bug spec when filing through `/bug`.

---

## Phase 6: Document

Update `docs/bluetooth.md`:
- Supported adapter requirements (BLE 4.2+ for LE Secure Connections; reject 4.0 controllers).
- Pairing UX: what the user sees on both devices, time limits, abort path.
- Revocation: how to revoke a paired phone from the CLI.
- Troubleshooting matrix mirroring the failure cases in Phase 4, with the syslog marker each one produces.

---

## Common Failure Modes

| Symptom | Likely cause |
|---------|--------------|
| Pairing succeeds, unlock fails | Code reads `ProvisionalBonded` state during auth. See Phase 2. |
| Unlock works once after pairing, then fails | Bond key persisted in process memory only; not written to keyring. |
| Random `bad_sig` failures | Clock skew makes nonce TTL evict too aggressively. Or HMAC computed over wrong field order. |
| Phone "found" but never connects | `Adapter1.Powered=false` after a kernel suspend; need to re-enable on resume. |
| Unlock takes > 5s on first attempt | Service discovery on every call; cache the characteristic handle per session. |

---

<self_check>

Before closing a BT-touching task:

- Is every frame format documented (version, nonce, payload, tag)?
- Is the pairing state machine explicit, with every transition gated?
- Are tests run against the mock peer, not the host's real BT adapter?
- Does the test matrix include at least one replay test and one timeout test?
- Does production code never read from `ProvisionalBonded` during unlock?
- Is the bond key stored in the system keyring, not a flat file?

</self_check>

<rules>

1. Proximity is necessary but not sufficient. Every unlock requires a fresh challenge plus a valid authenticator.
2. The state machine is explicit. No "is_paired" booleans — use the `PairingState` enum.
3. Mock at the BlueZ trait boundary, never above it.
4. Reject the frame before parsing it if version/nonce/tag are wrong.
5. Bond keys live in the kernel keyring or `libsecret`, never in plaintext files.
6. Every test asserts a specific syslog marker — log lines are part of the contract.

</rules>
