//! BLAKE3-keyed-hash MAC over the wire-format body, truncated to 16 bytes,
//! with constant-time verification.
//!
//! See `specs/syauth/SPEC.md` §4.1 (dataflow) and §6 (threat model T-010).
//! See `specs/journeys/JOURNEY-S-004-crypto-primitives.md` for the rationale
//! behind the 16-byte truncation choice and the constant-time guarantee.
//!
//! Public surface:
//! - [`compute_tag`] — `(bond_key, body) -> [u8; TAG_LEN]`.
//! - [`verify_tag`] — `(bond_key, body, tag) -> bool`, routed through
//!   `subtle::ConstantTimeEq::ct_eq` so verification time does not leak the
//!   per-byte position of a mismatch.

use subtle::ConstantTimeEq;

use crate::frame::TAG_LEN;

/// Length in bytes of a per-bond symmetric key. BLAKE3 keyed-hash mandates
/// exactly 32 bytes of key (per the BLAKE3 spec, §3.2).
pub const BOND_KEY_BYTES: usize = 32;

/// Re-export of the frame-layer tag length so callers do not need to import
/// it from two places. The value is the canonical 16-byte truncation
/// documented in the journey doc.
pub use crate::frame::TAG_LEN as MAC_TAG_LEN;

/// Compute the BLAKE3-keyed-hash MAC over `frame_body`, truncated to the
/// first [`TAG_LEN`] bytes of the 32-byte BLAKE3 output.
///
/// `bond_key` is the per-bond symmetric key (32 bytes, derived at pairing
/// time and persisted under secrets-storage per S-006).
///
/// `frame_body` is the wire-format encoding of the frame *without* the
/// trailing tag suffix — i.e. `[version:1] || [nonce:16] || [payload:N]`.
/// Use [`crate::frame::Frame::body_bytes`] to produce it from a parsed frame.
///
/// Truncation rationale: BLAKE3's 256-bit output remains a strong MAC under
/// truncation; 128 bits of MAC strength is ample for the syauth threat
/// model (BLE-bounded attacker, a few attempts per second, fresh per-session
/// keying). Full 32 bytes would double on-wire overhead with no real-world
/// gain. Documented in `JOURNEY-S-004-crypto-primitives.md`.
pub fn compute_tag(bond_key: &[u8; BOND_KEY_BYTES], frame_body: &[u8]) -> [u8; TAG_LEN] {
    let hash = blake3::keyed_hash(bond_key, frame_body);
    let full = hash.as_bytes();
    let mut tag = [0u8; TAG_LEN];
    tag.copy_from_slice(&full[..TAG_LEN]);
    tag
}

/// Verify a 16-byte MAC tag against a freshly-computed one in constant
/// time.
///
/// Returns `true` iff `tag` equals `compute_tag(bond_key, frame_body)`.
///
/// All byte comparisons are routed through `subtle::ConstantTimeEq::ct_eq`
/// so the verification time does not depend on the position of the first
/// differing byte. Defends T-010 (timing side channels) per SPEC §6 — without
/// it, an attacker on a noisy LAN could in principle binary-search the tag
/// by submitting forged tags and measuring how long verification takes.
///
/// The length comparison is also implicit in the API (the parameter is
/// `&[u8; TAG_LEN]`, a compile-time-fixed length), so the early-out below
/// is belt-and-braces — it does not leak because the length is a compile-
/// time constant on both sides.
pub fn verify_tag(bond_key: &[u8; BOND_KEY_BYTES], frame_body: &[u8], tag: &[u8; TAG_LEN]) -> bool {
    let computed = compute_tag(bond_key, frame_body);
    // Belt-and-braces: both sides are `[u8; TAG_LEN]`, so this is a
    // compile-time guarantee. Comparing `computed.len() != tag.len()` would
    // be unreachable; we instead rely on `ct_eq` over fixed-length arrays,
    // which itself walks every byte regardless of mismatch position.
    computed.ct_eq(tag).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 32-byte bond key used across the mac-layer unit tests. Pinned so the
    /// failure mode of `compute_tag` against this exact key is easy to
    /// diff against the KAT JSON file.
    const BOND_KEY_FIXTURE: [u8; BOND_KEY_BYTES] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16,
        0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20,
    ];

    /// A second bond key used to exercise `verify_rejects_wrong_bond_key`.
    const OTHER_BOND_KEY_FIXTURE: [u8; BOND_KEY_BYTES] = [0xff; BOND_KEY_BYTES];

    /// Index of the byte to flip in `verify_rejects_bit_flipped_tag`.
    const FLIPPED_BYTE_INDEX: usize = 0;

    /// XOR mask used to flip a single bit cheaply.
    const FLIP_MASK: u8 = 0x01;

    #[test]
    fn compute_then_verify_roundtrip() {
        let body: &[u8] = b"version-nonce-challenge-bytes-go-here-1234567890";
        let tag = compute_tag(&BOND_KEY_FIXTURE, body);
        assert!(verify_tag(&BOND_KEY_FIXTURE, body, &tag), "freshly-computed tag must verify");
    }

    #[test]
    fn compute_tag_is_deterministic() {
        let body: &[u8] = b"deterministic";
        let a = compute_tag(&BOND_KEY_FIXTURE, body);
        let b = compute_tag(&BOND_KEY_FIXTURE, body);
        assert_eq!(a, b, "BLAKE3 keyed-hash is deterministic over a fixed (key, message)");
    }

    #[test]
    fn compute_tag_returns_tag_len_bytes() {
        let tag = compute_tag(&BOND_KEY_FIXTURE, b"");
        assert_eq!(tag.len(), TAG_LEN);
    }

    #[test]
    fn verify_rejects_bit_flipped_tag() {
        let body: &[u8] = b"some-payload";
        let mut tag = compute_tag(&BOND_KEY_FIXTURE, body);
        tag[FLIPPED_BYTE_INDEX] ^= FLIP_MASK;
        assert!(!verify_tag(&BOND_KEY_FIXTURE, body, &tag), "bit-flipped tag must be rejected");
    }

    #[test]
    fn verify_rejects_wrong_bond_key() {
        let body: &[u8] = b"some-payload";
        let tag = compute_tag(&BOND_KEY_FIXTURE, body);
        assert!(
            !verify_tag(&OTHER_BOND_KEY_FIXTURE, body, &tag),
            "tag computed under bond_key_a must not verify under bond_key_b"
        );
    }

    #[test]
    fn verify_rejects_bit_flipped_body() {
        let mut body = b"some-payload".to_vec();
        let tag = compute_tag(&BOND_KEY_FIXTURE, &body);
        body[0] ^= FLIP_MASK;
        assert!(
            !verify_tag(&BOND_KEY_FIXTURE, &body, &tag),
            "tag must not verify if the body changed"
        );
    }

    #[test]
    fn verify_is_constant_time_smoke() {
        // Behavioral smoke for the constant-time-compare path: with a
        // known-bad (all-zero) tag, `verify_tag` returns false. The timing
        // claim itself rests on the `subtle::ConstantTimeEq::ct_eq`
        // contract, documented in the journey doc.
        let body: &[u8] = b"timing-smoke";
        let bad_tag = [0u8; TAG_LEN];
        assert!(!verify_tag(&BOND_KEY_FIXTURE, body, &bad_tag));
    }

    #[test]
    fn empty_body_still_produces_a_valid_tag() {
        let tag = compute_tag(&BOND_KEY_FIXTURE, b"");
        assert!(verify_tag(&BOND_KEY_FIXTURE, b"", &tag));
    }
}
