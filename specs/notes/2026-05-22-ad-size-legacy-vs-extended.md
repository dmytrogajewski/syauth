# 2026-05-22 — LE advertisement payload exceeds 31-byte legacy budget

## Context
While investigating BUG-20260522-0138 (`SyauthCompanionService` zombie after `START_STICKY` resurrect), a `btmon -w` capture of the host's hci0 showed that bluez is using **Extended Advertising PDUs** (`LE Set Extended Advertising Parameters/Data/Enable`, HCI opcodes `0x08|0x0036/0x0037/0x0039`) for the syauth advertisement, rather than legacy `ADV_IND`. The PDU type is set to `Properties: 0x0001` (Connectable) — **without** the legacy-PDU bit `0x10` — so the on-air format is true Extended Advertising, not a legacy-compatible extended emission.

This was not the root cause of BUG-20260522-0138 — the actual fix landed on the Android side (`SyauthCompanionService.ensureDefaultGattClientFactory`). It is, however, a latent hygiene issue worth documenting and eventually fixing.

## Observation
The captured advertising data is 45 bytes:

```
21 07 <16 B UUID1> <16 B UUID2>     ← 2× 128-bit Service UUIDs (Complete) = 34 B
07 09 73 79 61 75 74 68             ← Complete Local Name "syauth" = 8 B
02 01 06                            ← Flags (LE General Discoverable + BR/EDR Not Supported) = 3 B
                                    Total = 45 B
```

The legacy advertising payload ceiling is **31 bytes**. With the user's single bond:
1. `build_uuid_union` (`crates/syauth-presenced/src/orchestrator.rs:1136`) returns the per-peer rotating UUID **plus** the pair-mode UUID → 2 × 16 = 32 B of UUIDs alone.
2. `build_advertisement` (`crates/syauth-transport/src/peripheral.rs:623`) appends `local_name = "syauth"` and `discoverable = true` (which yields the Flags AD type).

That structurally exceeds 31 B as soon as ≥1 bond exists. BlueZ silently falls back to extended PDUs in that case.

## Why it didn't bite us today
On the user's S25 Ultra, both the `BluetoothGatt(autoConnect=true)` background acceptor and Samsung's `LE Enhanced Connection Complete` path appear to handle extended PDUs correctly. So once the phone-side `PersistentGattClient` is alive (the fix from BUG-20260522-0138), reconnection works fine against extended-PDU advertisements.

The two cases where the legacy/extended mismatch could bite us:
- **OEMs whose autoConnect background scanner is legacy-only.** Plausible on older Pixel firmwares and on aftermarket Android stacks. We have no measurements; field exposure depends on user device mix.
- **CDM-driven `startObservingDevicePresence` callbacks.** The OS-owned scan filter we observed via `dumpsys bluetooth_manager` was registered with `CB Leg` (legacy callback type). If we ever route the autoConnect through CDM presence (rather than the explicit foreground service holding `BluetoothGatt(autoConnect=true)`), this mismatch becomes load-bearing.

## Proposed remediation
Three options, in increasing engineering cost:

1. **Drop the pair UUID from the advertisement when ≥1 bond exists.** Lowest cost, host-side only. Touchpoint: `orchestrator.rs::build_uuid_union`. Side effect: pairing a *second* phone requires explicit pair-mode entry on the host. Acceptable given the rarity of multi-device pairing; SPEC §3.2 D8 already allows this trade-off.

2. **Drop the `local_name` field from the primary advertisement.** Saves 8 bytes, takes the payload from 45 → 37 B. Still over the 31-byte budget. Useful only combined with (1) — together they bring a 1-bond advertisement to ~20 B, comfortably legacy-compatible.

3. **Split UUIDs across primary advertisement + scan response.** Each can hold 31 bytes under legacy. Cleanest spec-correct solution. Requires patching `bluer` 0.17's `Advertisement` struct — it currently lacks a `scan_response_data` field even though BlueZ over D-Bus accepts it. Higher engineering cost: fork/patch of an external crate; coordinate upstream PR for long-term hygiene.

Recommendation: (1) + (2) together as a single small change in `syauth-presenced`. Defer (3) until a second OEM with legacy-only background scanning shows up in field reports.

## Acceptance check
After landing (1) + (2), `sudo btmon -w` during one rotation cycle should show:
- `Properties: 0x0013` (Connectable + Scannable + **Legacy**) — confirming legacy PDU.
- Total advertising data length ≤ 31 B.
- Phone re-acquires within the normal autoConnect window (≤ a few seconds) after `systemctl --user restart syauth-presenced.service`.

## Cross-references
- BUG-20260522-0138 (actual incident; phone-side fix already shipped)
- `crates/syauth-transport/src/bluez_advertise.rs::ADVERTISE_LOCAL_NAME` (the constant we'd drop)
- `crates/syauth-presenced/src/orchestrator.rs::build_uuid_union` (where the pair UUID gets unconditionally merged)
- bluer 0.17.4 `src/adv.rs` — confirms no `scan_response_data` field at struct level
