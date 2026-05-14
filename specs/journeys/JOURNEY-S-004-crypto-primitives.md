# JOURNEY-S-004: `syauth-core` Ed25519 signing & BLAKE3-keyed-hash tag

## Roadmap Link
- Source roadmap: [specs/syauth/ROADMAP.md §S-004](../syauth/ROADMAP.md)
- Feature: ship the crypto layer of the syauth protocol — Ed25519 signing of
  `version || nonce || challenge` plus a 16-byte BLAKE3-keyed-hash MAC over
  the same bytes, with every comparison routed through
  `subtle::ConstantTimeEq` so verification time does not leak the tag.

## 1. Journey

When **I am the `syauth-pam` author implementing `pam_sm_authenticate` in
S-009**, I want **`syauth_core::sign::{sign_frame, verify_frame}` plus
`syauth_core::mac::{compute_tag, verify_tag}` to take the parsed `Frame`
from S-002 and produce / check a deterministic Ed25519 signature and a
16-byte BLAKE3-keyed-hash tag, with all byte comparisons in constant time**,
so I can **defeat T-010 (timing side channels) by construction, defeat
classic forgery and bit-flip-on-the-wire attacks with strong primitives
already vetted by RustCrypto, and ship a primitive layer whose KAT vectors
are pinned in `testdata/kat.json` so any silent re-derivation regression is
caught at `cargo test` time**.

## 2. CJM

The downstream consumer is the PAM module author one roadmap step ahead
(S-009). Today, S-002 leaves them with a `Frame` that has a
placeholder all-zero `[u8; TAG_LEN]` tag and no signature surface at all.
The SPEC §4.1 dataflow demands:

1. The desktop signs `[ver=1][nonce][challenge]` with its Ed25519 host key
   (the public counterpart pinned in the bond record on the phone) and
   appends a BLAKE3-keyed-hash MAC over the same bytes using the
   per-bond symmetric key.
2. The phone verifies both the signature and the MAC before responding.
3. Both sides reject any mismatch in constant time so an attacker cannot
   derive bytes of the tag from timing differences (T-010).

The PAM author needs:

- A signing API that takes `&SigningKey` + `&Frame` and returns
  `Signature` — no `Result`, because every input that satisfies the type
  has a valid signature (Ed25519 is total over `Vec<u8>` messages).
- A verification API that takes `&VerifyingKey` + `&Frame` + `&Signature`
  and returns `Result<(), VerifyError>` where `VerifyError` carries
  enough to decide PAM return code (`Signature` → `PAM_AUTH_ERR`,
  `BadEncoding` → `PAM_AUTH_ERR`).
- A MAC API that operates on raw bytes (`&[u8; BOND_KEY_BYTES]` +
  `&[u8]`) so the caller can MAC the wire-shaped body without
  re-paying for an extra encode pass.
- A constant-time tag check whose API surface is `bool` and whose
  implementation routes through `subtle::ConstantTimeEq::ct_eq` — no
  short-circuit `==`. Length mismatch (impossible with the const-sized
  type signatures, but defended in depth) short-circuits to `false`
  before the ct_eq call; the length itself is a compile-time constant
  so the early-out does not leak.

### Phase 1: Sign the challenge

**User Intent:** The PAM module on the desktop has built a `Frame { version,
nonce, payload }` where `payload` is the challenge bytes. It wants to sign
exactly `version || nonce || payload` with its host Ed25519 key.

**Actions:** Call `sign_frame(&signing_key, &frame) -> Signature`. The
implementation extracts `frame.body_bytes()` (= `[version:1] || [nonce:16]
|| [payload:N]` — i.e., the wire frame without the trailing 16-byte tag)
and passes it to `SigningKey::sign`.

**Pain / Risk:**
- Caller could be tempted to sign `Frame::encode` output (which contains
  the placeholder tag). The signed-message contract is body-only so the
  tag can be filled in *after* signing without invalidating the
  signature. Documented as `body_bytes` and used by both `sign_frame`
  and `compute_tag` to keep a single source of truth.
- Ed25519 in `ed25519-dalek` v2 is deterministic by default (RFC 8032
  Section 5.1.6 nonce derivation), so a fixed signing key and a fixed
  body produce a fixed signature. The KAT vectors rely on this.
- The `Signature` type is `Copy`/`Clone` and 64 bytes wide
  (`SIGNATURE_LEN`); the body of the response frame will carry it as a
  raw payload, no length prefix.

**Success Signal:** `verify_frame(&verifying_key, &frame, &sig)` returns
`Ok(())`.

### Phase 2: Verify the response signature

**User Intent:** The PAM module receives a response `Frame` from the phone
and wants to reject any forgery, bit-flip, or wrong-pubkey use.

**Actions:** Call `verify_frame(&pubkey, &frame, &sig)`. The implementation
reconstructs the body and calls `VerifyingKey::verify_strict` (strict
verification rejects malleable signatures per the ed25519-dalek docs).

**Pain / Risk:**
- A bit-flipped signature must produce `Err(VerifyError::Signature(_))`
  — exercised by `verify_rejects_bit_flipped_signature`.
- A wrong pubkey (correct signature from a different key) must also
  produce `Err(VerifyError::Signature(_))` — exercised by
  `verify_rejects_wrong_pubkey`.
- A bit-flipped *body* (e.g. attacker mutates the nonce) likewise fails;
  conceptually covered by the bit-flipped-signature case because the
  signature is bound to the exact body bytes.

**Success Signal:** Negative tests all return the typed `VerifyError`
variant; positive test returns `Ok(())`.

### Phase 3: Compute and verify the MAC tag

**User Intent:** Bind every frame to the bond's symmetric key in addition to
the asymmetric signature. The MAC is what the relay attacker fails to
forge without the bond key — even if they capture an old signature, they
cannot resign with a fresh nonce.

**Actions:**
- `compute_tag(bond_key, body_bytes) -> [u8; TAG_LEN]` invokes
  `blake3::keyed_hash(bond_key, body_bytes)` and truncates the 32-byte
  output to 16 bytes by taking the first half.
- `verify_tag(bond_key, body_bytes, tag) -> bool` recomputes, then routes
  the comparison through `subtle::ConstantTimeEq::ct_eq`. No `==`.

**Pain / Risk:**
- Truncation to 16 bytes: BLAKE3 is a 256-bit hash whose security claim
  for keyed-hash output truncation is "as strong as the truncated
  length" (per the BLAKE3 spec, §3.2). 128 bits of MAC strength is
  ample against the syauth threat model (BLE-bounded attacker, a few
  attempts per second, per-session keying material). Full 32 bytes
  would double on-wire overhead with no real-world gain.
- Constant-time compare defends T-010 (SPEC §6) — without it, an
  attacker on a noisy LAN could in principle measure how many leading
  bytes of a forged tag match the real one and binary-search the tag.
  `subtle::ConstantTimeEq` is the documented mitigation.
- Length-mismatch early-out: API takes `&[u8; TAG_LEN]` so the type
  system already guarantees length. A belt-and-braces early-out in
  `verify_tag` compares lengths first; the length is a compile-time
  constant so this does not leak.

**Success Signal:** `verify_tag` returns `true` for matching tag, `false`
for any mismatch. KAT vectors pin both the `expected_tag` and the
`expected_signature` byte-for-byte.

### Phase 4: Pin behavior with KAT vectors

**User Intent:** A future maintainer who refactors the crypto module must
not be able to silently change either the BLAKE3 truncation or the Ed25519
body framing. The KAT JSON file is the regression net.

**Actions:** `crates/syauth-core/testdata/kat.json` ships three vectors:
- `kat-01-empty-payload` — empty challenge, fixed bond + signing keys.
- `kat-02-typical-32b-payload` — 32-byte fake challenge (the size of an
  Ed25519 public key, a realistic upper bound for the desktop→phone
  direction).
- `kat-03-max-payload` — `MAX_PAYLOAD_LEN = 4096`-byte payload (`0xAA` *
  4096). Exercises the BLAKE3 streaming-vs-one-shot edge.

The `expected_tag` and `expected_signature` were generated by a
`#[cfg(test)]` helper inside the crate that calls our own
`compute_tag` and `sign_frame` once, prints the hex outputs, and then
those outputs are pinned in the JSON. The integration test
`tests/kat.rs` re-parses the JSON and asserts byte-equality against
fresh computations. Because BLAKE3 keyed-hash and Ed25519 (with the
strict, deterministic signing of `ed25519-dalek` v2) are both
deterministic over a fixed key + message, the assertion is reproducible
on every host.

**Pain / Risk:**
- Hex encoding: lowercase, no `0x` prefix, no separators. Enforced by
  the parsing helper in `tests/kat.rs`.
- The KAT file's top-level `_doc` field names the encoding convention so
  no future contributor has to guess.

**Success Signal:** `cargo test -p syauth-core --test kat` passes; the
three vectors are loaded and verified.

### Friction and Opportunity

| Friction | Phase | Opportunity |
|----------|-------|-------------|
| Two callers (signing + MAC) want to view the frame as "body only" but `Frame::encode` includes the tag | 1, 3 | `Frame::body_bytes()` helper that emits exactly `version || nonce || payload`, used by both crypto sites |
| Hand-written hex in JSON is error-prone | 4 | KAT loader rejects upper-case and non-hex with a typed error; `_doc` key names the convention |
| Future maintainer regresses `verify_tag` to `==` | 3 | `subtle::ConstantTimeEq::ct_eq` is the only API surface used inside `verify_tag`; a future regression is caught by `make lint` (clippy `redundant_pattern_matching`-style guards) and `verify_is_constant_time_smoke` is a smoke that exercises the function on a known-bad tag |
| Caller forgets that Ed25519 verify is *strict* in v2 | 2 | `verify_frame` uses `verify_strict` directly; no caller is asked to remember |

### North Star Summary

The PAM module author, after S-004, can `use syauth_core::{sign, mac};`,
build a `Frame`, call `sign_frame` to mint the response signature, call
`compute_tag` to seal it with the bond key, ship the result on the wire,
and on the verifier side reverse both operations in constant time with
typed errors that map cleanly to PAM return codes. The KAT JSON is the
golden record that proves the protocol bytes have not silently moved.

## 3. UX Implementation and Assessment

### Time to First Value
- [x] A caller can sign + tag a frame in two function calls totaling under
  10 lines of glue (`sign_frame(&sk, &f)`, `compute_tag(bk, &body)`).
- [x] Onboarding: one journey doc, one KAT file with `_doc` key, three
  vectors. New maintainer learns the contract in one read.

### Onboarding Clarity
- [x] Public surface is discoverable via `pub use crate::sign::*; pub use crate::mac::*;` in `lib.rs`.
- [x] Error variants name the failure: `VerifyError::Signature(_)`,
  `VerifyError::BadEncoding(FrameError)`.

### Production-Ready Defaults
- [x] No `Result` on `sign_frame` — Ed25519 is total over the input.
- [x] No `Result` on `compute_tag` — BLAKE3 keyed-hash is total over any
  byte slice with a 32-byte key.

### Golden Path Quality
- [x] Roundtrip test `sign_then_verify_roundtrip` in `sign.rs::tests`.
- [x] Roundtrip test `compute_then_verify_roundtrip` in `mac.rs::tests`.
- [x] KAT-level golden tests in `tests/kat.rs`.

### Decision Load
- [x] One signing function, one verification function, one tag-compute
  function, one tag-verify function. No flags, no modes.

### Progressive Complexity
- [x] Body-byte view is a single helper on `Frame` (`body_bytes`), so a
  caller who only needs to MAC a frame does not have to know about
  `Frame::encode`'s tag suffix.

### Error Quality
- [x] `VerifyError::Signature` carries the underlying
  `ed25519_dalek::SignatureError` (which has a `Display`).
- [x] `VerifyError::BadEncoding` carries the underlying `FrameError`.

### Failure Safety
- [x] Constant-time tag compare via `subtle::ConstantTimeEq` defends T-010.
- [x] `verify_frame` uses `verify_strict` to reject malleable signatures
  (RFC 8032 §8.4 cofactored verification ambiguity).

### Runtime Transparency
- [x] No logging in this layer — the caller decides what to log; nonces
  and tags must never appear in logs (SPEC §4.2 reliability bullet).

### Debuggability
- [x] KAT JSON file is human-readable hex; a `cargo test` failure prints
  the vector `name`.

### Cross-Surface Consistency
- [x] Same Rust crate compiles into `syauth-pam` (desktop) and
  `syauth-mobile` (UniFFI for Android). No re-implementation in Kotlin.

### Workflow Consistency
- [x] `make lint` + `make test` are the only quality gates, same as every
  other crate.

### Change Safety
- [x] Three KAT vectors pin the byte layout; any silent crypto-layer
  change breaks the test.

### Experimentation Safety
- [x] Pure functions over inputs; no global state.

### Interaction Latency
- [x] BLAKE3-keyed-hash and Ed25519 are both sub-millisecond on a modern
  CPU. The PAM golden-path budget (< 2.0 s wall clock, SPEC §4.2) is
  dominated by BLE round-trips, not crypto.

### Developer Feedback Speed
- [x] `cargo test -p syauth-core` runs the in-file tests + KAT integration
  test in well under a second.

### Team Scale
- [x] No new third-party deps beyond `ed25519-dalek`, `subtle`,
  `serde_json` (dev), and `hex` (dev). All RustCrypto-vetted.

### System Scale
- [x] No allocation in `compute_tag` or `verify_tag`; one heap alloc in
  `sign_frame` for the body view (could be optimized later — not on
  the hot path for v0.1).

### Right Behavior by Default
- [x] No way to disable the constant-time check from outside the crate.

### Anti-Bypass Design
- [x] `verify_tag` returns `bool`. There is no API to extract the
  per-byte XOR difference or to learn how many leading bytes matched.

## 4. Tests

### TC-01: `sign_then_verify_roundtrip`

**Given** a fresh `SigningKey` from a fixed 32-byte seed and a `Frame`
with a non-empty payload.
**When** `sign_frame(&sk, &frame)` produces a `Signature`, then
`verify_frame(&sk.verifying_key(), &frame, &sig)` is called.
**Then** the result is `Ok(())`.

### TC-02: `verify_rejects_bit_flipped_signature`

**Given** a valid `(frame, sig)` pair.
**When** byte 0 of `sig` is XOR'd with `0x01` and `verify_frame` is called.
**Then** the result is `Err(VerifyError::Signature(_))`.

### TC-03: `verify_rejects_wrong_pubkey`

**Given** a valid `(frame, sig)` pair signed by `sk_a`.
**When** `verify_frame(&sk_b.verifying_key(), &frame, &sig)` is called
with a different signing key's pubkey.
**Then** the result is `Err(VerifyError::Signature(_))`.

### TC-04: `compute_then_verify_roundtrip`

**Given** a 32-byte bond key and a 64-byte body.
**When** `compute_tag(&bk, &body)` returns a 16-byte tag and
`verify_tag(&bk, &body, &tag)` is called.
**Then** the return is `true`.

### TC-05: `verify_rejects_bit_flipped_tag`

**Given** a valid `(bk, body, tag)` triple.
**When** byte 0 of `tag` is XOR'd with `0x01` and `verify_tag` is called.
**Then** the return is `false`.

### TC-06: `verify_rejects_wrong_bond_key`

**Given** a valid `(bk_a, body, tag)` triple.
**When** `verify_tag(&bk_b, &body, &tag)` is called with a different
bond key.
**Then** the return is `false`.

### TC-07: `verify_is_constant_time_smoke`

**Given** any body and a tag of zero bytes.
**When** `verify_tag` is called with a non-matching computed tag.
**Then** the return is `false`. (This is a behavioral smoke; the
constant-time claim is documented to rest on the `subtle` crate's
`ct_eq` contract.)

### TC-08: `kat_file_loads_and_verifies_byte_for_byte`

**Given** `crates/syauth-core/testdata/kat.json` with three vectors.
**When** the integration test `tests/kat.rs` parses each vector,
computes the tag, signs the frame, and compares against the pinned
`expected_tag` / `expected_signature`.
**Then** every vector matches byte-for-byte; `verify_frame` and
`verify_tag` accept the pinned values.

### How KAT vectors were generated

Step-by-step:

1. The crate authors a `#[cfg(test)] fn kat_bootstrap_vectors()` helper in
   `tests/kat.rs` (guarded by `#[ignore]` so it does not run in normal
   test runs).
2. The helper synthesizes the three inputs (`bond_key`, `signing_key`,
   `nonce`, `payload`) deterministically from pinned hex seeds.
3. For each input, the helper calls the just-implemented `compute_tag`
   and `sign_frame`, then prints the resulting hex.
4. The author copies the printed hex into `kat.json` once, by hand.
5. From that point forward, `tests/kat.rs::kat_file_loads_and_verifies`
   re-parses the JSON and asserts the same values are still produced by
   `compute_tag` and `sign_frame`. Any drift (e.g. someone swaps the
   BLAKE3 truncation from "first 16 bytes" to "last 16 bytes") is
   caught immediately.

This is the standard "test vector bootstrap" pattern used by RustCrypto;
the alternative (cross-validate against a reference impl in another
language) is excessive for our threat model because BLAKE3 and Ed25519
are themselves heavily cross-tested upstream — the KAT here defends
*our* framing of the inputs, not the primitives.

## Traceability
- Roadmap item: [specs/syauth/ROADMAP.md §S-004](../syauth/ROADMAP.md)
- Implementation files: `crates/syauth-core/src/sign.rs`,
  `crates/syauth-core/src/mac.rs`, `crates/syauth-core/src/frame.rs`
  (Frame::body_bytes helper), `crates/syauth-core/src/lib.rs`
  (re-exports), `crates/syauth-core/Cargo.toml` (deps).
- Test files: `crates/syauth-core/src/sign.rs::tests`,
  `crates/syauth-core/src/mac.rs::tests`,
  `crates/syauth-core/tests/kat.rs`,
  `crates/syauth-core/testdata/kat.json`.
