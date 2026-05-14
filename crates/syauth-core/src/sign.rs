//! Ed25519 signing & verification over the wire-format frame body.
//!
//! See `specs/syauth/SPEC.md` §4.1 (dataflow) and `specs/journeys/
//! JOURNEY-S-004-crypto-primitives.md` for the contract: the signed
//! message is exactly `version || nonce || challenge` — i.e. the frame's
//! body bytes as produced by [`crate::frame::Frame::body_bytes`].
//!
//! Public surface (re-exported at the crate root):
//! - [`SigningKey`], [`VerifyingKey`], [`Signature`] — re-exports of the
//!   `ed25519-dalek` types so callers do not have to add a second dep.
//! - [`sign_frame`] / [`verify_frame`].
//! - [`VerifyError`].

use ed25519_dalek::Signer;
pub use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
use thiserror::Error;

use crate::frame::{Frame, FrameError};

/// Number of bytes the prefix of the signed message occupies (version +
/// nonce). The full signed message is this prefix followed by the
/// challenge payload.
pub const SIGNED_MESSAGE_PREFIX_LEN: usize = crate::frame::VERSION_LEN + crate::frame::NONCE_LEN;

/// Length in bytes of an Ed25519 signature. Pinned as a named constant per
/// the AGENTS.md "magic numbers" rule.
pub const SIGNATURE_LEN: usize = ed25519_dalek::SIGNATURE_LENGTH;

/// Errors produced by [`verify_frame`].
///
/// `Signature` carries the underlying `ed25519_dalek::SignatureError` so
/// the caller (`pam_sm_authenticate` in S-009) can decide whether to log
/// the specifics or coalesce to `PAM_AUTH_ERR`.
///
/// `BadEncoding` carries the underlying [`FrameError`] for the case where
/// the body bytes themselves cannot be reconstituted from the parsed
/// `Frame` — i.e. the caller constructed a `Frame` with a payload longer
/// than [`crate::frame::MAX_PAYLOAD_LEN`], which is rejected before any
/// crypto runs.
#[derive(Debug, Error)]
pub enum VerifyError {
    /// Ed25519 strict verification failed: the signature does not match
    /// the (pubkey, body) pair. Could mean a forgery, a bit-flipped
    /// signature, a wrong pubkey, or a tampered body.
    #[error("ed25519 signature verification failed: {0}")]
    Signature(#[from] ed25519_dalek::SignatureError),

    /// The `Frame` could not be re-encoded into its body view (e.g.
    /// payload too large). Inputs that fail this check never reach the
    /// crypto core.
    #[error("frame body encoding failed: {0}")]
    BadEncoding(#[from] FrameError),
}

/// Sign the frame's body bytes with `privkey` and return the 64-byte
/// Ed25519 signature.
///
/// Ed25519 signing in `ed25519-dalek` v2 is deterministic (RFC 8032
/// §5.1.6), so a fixed signing key and a fixed body always produce the
/// same signature. The KAT vectors in `crates/syauth-core/testdata/
/// kat.json` rely on this.
///
/// # Errors
///
/// Returns `Err(FrameError::BadLength)` if the caller built a `Frame`
/// with a payload exceeding [`crate::frame::MAX_PAYLOAD_LEN`]. Every
/// other input that satisfies the type signature is signable.
pub fn sign_frame(privkey: &SigningKey, frame: &Frame) -> Result<Signature, FrameError> {
    let body = frame.body_bytes()?;
    Ok(privkey.sign(&body))
}

/// Verify `sig` is a valid Ed25519 signature of `frame.body_bytes()`
/// under `pubkey`.
///
/// Uses `VerifyingKey::verify_strict`, which rejects malleable signatures
/// (RFC 8032 §8.4 cofactored verification ambiguity) — important because
/// the syauth response frame is replay-cached on the nonce, not on the
/// signature; allowing two distinct valid signatures over the same body
/// would technically open a re-encoding window even though the nonce
/// cache stops the obvious case.
pub fn verify_frame(pubkey: &VerifyingKey, frame: &Frame, sig: &Signature) -> Result<(), VerifyError> {
    let body = frame.body_bytes()?;
    pubkey.verify_strict(&body, sig)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{MAX_PAYLOAD_LEN, NONCE_LEN, SYAUTH_WIRE_VERSION_V1, TAG_LEN};

    /// A pinned 32-byte signing key seed used across the sign-layer unit
    /// tests. Hex equivalent: `0102…20`.
    const SIGNING_KEY_SEED: [u8; 32] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16,
        0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20,
    ];

    /// A second signing key used to exercise `verify_rejects_wrong_pubkey`.
    const OTHER_SIGNING_KEY_SEED: [u8; 32] = [0xff; 32];

    /// Index of the byte to flip in the negative tests.
    const FLIPPED_BYTE_INDEX: usize = 0;

    /// XOR mask used to flip a single bit cheaply.
    const FLIP_MASK: u8 = 0x01;

    fn fixture_frame(payload_len: usize) -> Frame {
        Frame {
            version: SYAUTH_WIRE_VERSION_V1,
            nonce: [0xA5; NONCE_LEN],
            payload: vec![0x5A; payload_len],
            tag: [0x00; TAG_LEN],
        }
    }

    #[test]
    fn sign_then_verify_roundtrip() {
        let sk = SigningKey::from_bytes(&SIGNING_KEY_SEED);
        let pk = sk.verifying_key();
        let frame = fixture_frame(32);
        let sig = sign_frame(&sk, &frame).expect("sign");
        verify_frame(&pk, &frame, &sig).expect("verify");
    }

    #[test]
    fn sign_frame_is_deterministic_per_key_and_body() {
        let sk = SigningKey::from_bytes(&SIGNING_KEY_SEED);
        let frame = fixture_frame(16);
        let a = sign_frame(&sk, &frame).expect("sign a");
        let b = sign_frame(&sk, &frame).expect("sign b");
        assert_eq!(a.to_bytes(), b.to_bytes(), "Ed25519 signing in v2 is deterministic per RFC 8032");
    }

    #[test]
    fn signature_is_signature_len_bytes() {
        let sk = SigningKey::from_bytes(&SIGNING_KEY_SEED);
        let frame = fixture_frame(0);
        let sig = sign_frame(&sk, &frame).expect("sign");
        assert_eq!(sig.to_bytes().len(), SIGNATURE_LEN);
    }

    #[test]
    fn verify_rejects_bit_flipped_signature() {
        let sk = SigningKey::from_bytes(&SIGNING_KEY_SEED);
        let pk = sk.verifying_key();
        let frame = fixture_frame(8);
        let sig = sign_frame(&sk, &frame).expect("sign");
        let mut sig_bytes = sig.to_bytes();
        sig_bytes[FLIPPED_BYTE_INDEX] ^= FLIP_MASK;
        let bad_sig = Signature::from_bytes(&sig_bytes);
        let err = verify_frame(&pk, &frame, &bad_sig).expect_err("bit-flipped sig must be rejected");
        assert!(matches!(err, VerifyError::Signature(_)));
    }

    #[test]
    fn verify_rejects_wrong_pubkey() {
        let sk_a = SigningKey::from_bytes(&SIGNING_KEY_SEED);
        let sk_b = SigningKey::from_bytes(&OTHER_SIGNING_KEY_SEED);
        let pk_b = sk_b.verifying_key();
        let frame = fixture_frame(8);
        let sig = sign_frame(&sk_a, &frame).expect("sign");
        let err = verify_frame(&pk_b, &frame, &sig).expect_err("wrong pubkey must be rejected");
        assert!(matches!(err, VerifyError::Signature(_)));
    }

    #[test]
    fn verify_rejects_tampered_body() {
        let sk = SigningKey::from_bytes(&SIGNING_KEY_SEED);
        let pk = sk.verifying_key();
        let frame = fixture_frame(8);
        let sig = sign_frame(&sk, &frame).expect("sign");
        let mut tampered = frame.clone();
        tampered.nonce[0] ^= FLIP_MASK;
        let err = verify_frame(&pk, &tampered, &sig).expect_err("tampered body must be rejected");
        assert!(matches!(err, VerifyError::Signature(_)));
    }

    #[test]
    fn sign_rejects_oversized_payload() {
        let sk = SigningKey::from_bytes(&SIGNING_KEY_SEED);
        let frame = Frame {
            version: SYAUTH_WIRE_VERSION_V1,
            nonce: [0; NONCE_LEN],
            payload: vec![0; MAX_PAYLOAD_LEN + 1],
            tag: [0; TAG_LEN],
        };
        let err = sign_frame(&sk, &frame).expect_err("oversize must be rejected");
        assert_eq!(err, FrameError::BadLength);
    }

    #[test]
    fn verify_rejects_oversized_payload_as_bad_encoding() {
        let sk = SigningKey::from_bytes(&SIGNING_KEY_SEED);
        let pk = sk.verifying_key();
        // Sign a valid frame so the Signature bytes are well-formed; then
        // verify against an oversized frame to exercise the BadEncoding
        // branch.
        let valid = fixture_frame(0);
        let sig = sign_frame(&sk, &valid).expect("sign");
        let oversized = Frame {
            version: SYAUTH_WIRE_VERSION_V1,
            nonce: [0; NONCE_LEN],
            payload: vec![0; MAX_PAYLOAD_LEN + 1],
            tag: [0; TAG_LEN],
        };
        let err = verify_frame(&pk, &oversized, &sig).expect_err("oversize must fail BadEncoding");
        assert!(matches!(err, VerifyError::BadEncoding(FrameError::BadLength)));
    }
}
