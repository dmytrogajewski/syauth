# JOURNEY-S-002: `syauth-core` wire-format framing & parser

## Roadmap Link
- Source roadmap: [specs/syauth/ROADMAP.md §S-002](../syauth/ROADMAP.md)
- Feature: v1 wire-format frame encoder/decoder for `syauth-core`

## 1. Journey

When **I am a Rust engineer downstream of `syauth-core` (the PAM module in
S-008/S-009, the transport in S-007/S-010, the mobile UniFFI surface in
S-014)**, I want **a single, audited frame type that serializes and parses the
v1 syauth wire format defined in SPEC §3.3 / §4.2, complete with typed errors,
named offsets, property-based round-tripping, and a `cargo fuzz` target proving
the parser never panics on adversarial input**, so I can **wire protocol bytes
between the desktop and the phone without ever hand-rolling another `&[u8]`
slice or worrying that a malformed frame from a hostile peer aborts the PAM
process mid-`pam_sm_authenticate`**.

## 2. CJM

The downstream user is a Rust author who has just landed `syauth-core` as a
workspace dep and needs to send or receive a protocol frame. Today they have
nothing but the SPEC's prose layout `[ver:1][nonce:16][payload:N][tag:16]` and
the warning from AGENTS.md that any `panic!` in the PAM stack is fatal. This
journey gives them a `Frame` struct, an `encode` method that appends bytes to a
caller-supplied buffer, a `decode` function that returns a typed `FrameError`
on any malformed input, a `proptest` round-trip that proves encode/decode are
inverses, and a `cargo fuzz` target that exercises the parser against random
bytes without crashing.

### Phase 1: Construct

**User Intent:** Build a `Frame` from a known version, nonce, payload, and
tag, ready to hand to the transport layer.

**Actions:** Call `Frame { version: SYAUTH_WIRE_VERSION_V1, nonce, payload,
tag }`, where `nonce` is `[u8; NONCE_LEN]`, `payload` is a `Vec<u8>` of at most
`MAX_PAYLOAD_LEN` bytes, and `tag` is `[u8; TAG_LEN]`.

**Pain / Risk:**
- Mis-sized nonce or tag fields. Mitigated by fixed-length `[u8; N]` types —
  the type system rejects mis-sizing at compile time.
- Oversized payload at construction time. Caller is on the honor system; the
  encoder is the gatekeeper because an unbounded `Vec` field is the
  ergonomic choice. Encode rejects with `FrameError::BadLength`.
- Forgetting to use the `SYAUTH_WIRE_VERSION_V1` constant. Mitigated by
  having `decode` reject anything else explicitly, so a stray `0` or `2` is
  caught the moment it leaves an encoder.

**Success Signal:** `Frame` value compiles and the field types match the
constants documented at module level.

### Phase 2: Encode

**User Intent:** Serialize a frame to bytes for the transport.

**Actions:** Call `frame.encode(&mut buf)`. The encoder appends exactly
`HEADER_LEN + payload.len() + TAG_LEN` bytes, in order:
`[ver][nonce][payload][tag]`.

**Pain / Risk:**
- Oversized payload (`> MAX_PAYLOAD_LEN`): encoder returns
  `Err(FrameError::BadLength)` rather than producing a frame the decoder
  would reject. Early failure is cheaper than a round-trip rejection.
- Caller passes a non-empty `buf`: encoder *appends*, it does not overwrite.
  Documented in the rustdoc.

**Success Signal:** `buf.len() == HEADER_LEN + payload.len() + TAG_LEN`
and the prefix exactly matches `[ver][nonce]`, the middle is the payload, and
the suffix is the tag.

### Phase 3: Decode

**User Intent:** Take bytes off the wire and produce a `Frame` or a typed
error explaining exactly what's wrong.

**Actions:** Call `Frame::decode(&bytes)`.

**Pain / Risk:**
- Short read (`bytes.len() < HEADER_LEN + TAG_LEN`): returns
  `FrameError::TooShort { needed, got }` with `needed = HEADER_LEN + TAG_LEN`
  and `got = bytes.len()`. The numbers are useful for log lines and tests.
- Wrong version byte (`bytes[0] != SYAUTH_WIRE_VERSION_V1`): returns
  `FrameError::BadVersion(bytes[0])`. This is what catches the "phone speaks
  v2, desktop is still v1" case at the protocol layer; SPEC §4.5 mandates
  explicit rejection of unknown versions.
- Oversized frame (`payload.len() > MAX_PAYLOAD_LEN`): returns
  `FrameError::BadLength`. Caps the buffer the decoder is willing to allocate,
  giving us a hard DoS bound.
- Any other malformed input: cannot exist — the three error variants above
  exhaustively partition the failure space. The fuzzer in Phase 5 confirms it.

**Success Signal:** Decoded `Frame` equals the input to `encode`, byte for
byte; or a typed `FrameError` is returned (never a panic).

### Phase 4: Round-trip property

**User Intent:** Prove the encoder and decoder are inverses for every
well-formed frame.

**Actions:** `proptest` generates `Frame` values with payload length in
`0..=MAX_PAYLOAD_LEN`, encodes, decodes, asserts equality.

**Pain / Risk:**
- A property failure points to a real bug — flat encoder offsets, off-by-one
  on the tag slice, version drop. Caught before merge.

**Success Signal:** `proptest` runs the default 256 cases (configurable via
`PROPTEST_CASES`) with zero rejections.

### Phase 5: Fuzz

**User Intent:** Prove the decoder never panics on arbitrary bytes.

**Actions:** `cargo fuzz run frame_parse -- -runs=10000`.

**Pain / Risk:**
- Panic on out-of-bounds slice, integer overflow on length math, etc.
  Caught by libFuzzer immediately.

**Success Signal:** 10 000 iterations, zero crashes, zero leaks.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| Hand-rolled `&[u8]` indexing everywhere downstream | 2, 3 | Centralize in `Frame::encode/decode` so every consumer pays the audit cost once |
| Future v2 bump risks silent acceptance | 3 | `BadVersion(u8)` carries the offending byte so logs name the deviation |
| DoS via oversized frame allocations | 3 | `MAX_PAYLOAD_LEN` constant caps the decoder's appetite at 4096 bytes; documented |
| Endianness drift | 1, 2, 3 | Version byte is a single byte; no multi-byte integers ever cross the wire in v1, so endianness is moot. The `MAX_PAYLOAD_LEN` const carries a doc comment locking us to big-endian for any future multi-byte field. |
| Panic-in-PAM is fatal | 3, 5 | Fuzz target guarantees `decode` is total |

### North Star Summary

Every downstream consumer of `syauth-core` calls `Frame::encode` to put a
frame on the wire and `Frame::decode` to take one off. The function pair is
total (no panics, all paths typed), property-tested to be a round-trip, and
fuzzed to 10 000 iterations of adversarial bytes without crashing. No
hand-rolled byte indexing leaks out of `syauth-core`.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] A caller can encode and decode a `Frame` with one `use` and one method call.
- [x] Construction via struct literal — no builder boilerplate at this layer.

### Onboarding Clarity
- [x] The module-level rustdoc reproduces the wire-format diagram from SPEC §3.3.
- [x] Each `FrameError` variant's display string names the specific failure.

### Production-Ready Defaults
- [x] `MAX_PAYLOAD_LEN = 4096` chosen as the smallest power of two that
       comfortably exceeds an Ed25519 signature (64 B) + nonce (16 B) plus
       overhead. Sized so a 4-MTU BLE GATT exchange (~520 B writable each)
       can carry a full frame after fragmentation.
- [x] Endianness: any future multi-byte header field is big-endian; the
       `MAX_PAYLOAD_LEN` const carries a doc comment locking this in.

### Golden Path Quality
- [x] Round-trip property test passes for every payload length 0..=MAX_PAYLOAD_LEN.

### Decision Load
- [x] Caller only chooses `version`, `nonce`, `payload`, `tag`. Everything
       else (sizes, offsets, error variants) is fixed by the module.

### Progressive Complexity
- [x] Public surface: one struct, two free items (`encode`, `decode`),
       three error variants. Nothing else is `pub`.

### Error Quality
- [x] `TooShort { needed, got }` includes the deficit in numeric form so
       tests and logs can grep for it.
- [x] `BadVersion(u8)` carries the offending byte, not "0..255" or a string.
- [x] `BadLength` is a single variant; the payload-length check is the only
       length check the decoder owns beyond `TooShort`.

### Failure Safety
- [x] No `unwrap()` / `expect()` in production code. `unsafe_code = "deny"`
       inherited from workspace; not overridden in this crate.
- [x] Fuzz target proves the decoder is total.

### Runtime Transparency
- [x] `encode` returns `Result<(), FrameError>`; caller knows immediately
       if their frame is too big for the wire.

### Debuggability
- [x] `Frame` derives `Debug`, `PartialEq`, `Eq`, `Clone`.
- [x] `FrameError` derives `Debug`, `PartialEq`, `Eq`, `Clone`, and
       `thiserror::Error` Display.

### Cross-Surface Consistency
- [x] The same `Frame` type will be re-exported by `syauth-mobile` (S-014),
       so phone and desktop literally share the parser.

### Workflow Consistency
- [x] Tests live in `#[cfg(test)] mod tests` alongside the code, per
       AGENTS.md.

### Change Safety
- [x] Version byte is the change-safety primitive. Bumping the wire format
       in v2 will require a new `Frame` variant and explicit rejection of v1
       at the v2 boundary.

### Experimentation Safety
- [x] `proptest` cases are deterministic with a fixed seed when desired
       (via `PROPTEST_DEBUG_RNG=1`).

### Interaction Latency
- [x] `encode`/`decode` are allocation-light: `encode` only `extend`s the
       caller's buffer; `decode` allocates one `Vec<u8>` of payload bytes.

### Developer Feedback Speed
- [x] All five test cases (golden, round-trip, too-short, wrong-version,
       oversized) run in < 100 ms.

### Team Scale
- [x] Wire-format documentation lives in code (rustdoc) and in
       `specs/syauth/SPEC.md §3.3`; both are version-controlled.

### System Scale
- [x] `Frame::decode` is `O(n)` with a hard cap at `MAX_PAYLOAD_LEN`. No
       quadratic surprises as call volume scales.

### Right Behavior by Default
- [x] Decoder refuses unknown versions, oversized payloads, short reads, and
       nothing else — the three error variants exhaustively partition the
       failure space.

### Anti-Bypass Design
- [x] No "trust me" entry point. There is no `from_raw_parts` or `unsafe`
       constructor.

## 4. Tests

### TC-01: Golden encode

**Given** a frame `{ version: SYAUTH_WIRE_VERSION_V1, nonce: [1; 16],
payload: vec![0xAA; 4], tag: [0xBB; 16] }`.
**When** I call `encode(&mut buf)` on an empty `Vec<u8>`.
**Then** `buf` equals exactly the concatenation
`[0x01] || [0x01; 16] || [0xAA; 4] || [0xBB; 16]` — 37 bytes.

### TC-02: Round-trip property

**Given** any `Frame` with `version = SYAUTH_WIRE_VERSION_V1`, an arbitrary
16-byte nonce, a payload of 0..=`MAX_PAYLOAD_LEN` arbitrary bytes, and an
arbitrary 16-byte tag.
**When** I encode it, then decode the result.
**Then** the decoded `Frame` equals the input.

### TC-03: Too-short input

**Given** a byte slice of length `HEADER_LEN + TAG_LEN - 1` (32 bytes — one
byte short of the minimum frame).
**When** I call `Frame::decode`.
**Then** it returns `Err(FrameError::TooShort { needed: 33, got: 32 })`.

### TC-04: Wrong version

**Given** a 33-byte frame (minimum size) with the first byte set to `0x02`
(a hypothetical future version).
**When** I call `Frame::decode`.
**Then** it returns `Err(FrameError::BadVersion(0x02))`.

### TC-05: Oversized payload

**Given** a byte slice of length `HEADER_LEN + MAX_PAYLOAD_LEN + 1 + TAG_LEN`
(one byte past the maximum frame).
**When** I call `Frame::decode`.
**Then** it returns `Err(FrameError::BadLength)`.

### TC-06: Encode rejects oversized payload

**Given** a `Frame` constructed with a payload of `MAX_PAYLOAD_LEN + 1`
bytes.
**When** I call `encode`.
**Then** it returns `Err(FrameError::BadLength)` and leaves `buf` untouched
(idempotency on failure).

### TC-07: Fuzz parse

**Given** arbitrary `&[u8]` from libFuzzer.
**When** `Frame::decode(input)` is called.
**Then** it returns either `Ok(_)` or one of the three documented errors —
never panics, never aborts.

## Traceability
- Roadmap item: [specs/syauth/ROADMAP.md §S-002](../syauth/ROADMAP.md#step-s-002-syauth-core--wire-format-framing--parser)
- Implementation files: `crates/syauth-core/src/lib.rs`, `crates/syauth-core/src/frame.rs`, `crates/syauth-core/Cargo.toml`
- Test files: `crates/syauth-core/src/frame.rs` `#[cfg(test)] mod tests`, `crates/syauth-core/fuzz/fuzz_targets/frame_parse.rs`
