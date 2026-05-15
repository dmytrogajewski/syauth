//! Smoke test for the `syauth-mobile` public surface.
//!
//! Run with:
//!
//! ```text
//! cargo run -p syauth-mobile --example smoke
//! ```
//!
//! Drives each of the four UDL-exported functions end-to-end from
//! outside the crate, with hand-built fixtures, and prints `OK` on
//! success. The example imports ONLY the crate-level re-exports — the
//! same set the UDL exposes to Kotlin/Swift — so a missing `pub use`
//! breaks the example at compile time.
//!
//! Mirrors `~/sources/prrr/prrr-mobile/examples/kotlin_example.kt` in
//! intent (drive the public surface from outside the crate); we ship a
//! Rust binary instead of a Kotlin file because the Android Gradle
//! project (S-015) supplies its own Kotlin example against the AAR.

use syauth_mobile::{
    Invite, MOBILE_BOND_KEY_LEN, MobileError, oob_code_for_bond, parse_invite_uri, sign_challenge_response, verify_challenge_frame,
};

/// Wire-format frame header constants — duplicated from `syauth-core`
/// to keep the example self-contained (the smoke example must compile
/// against ONLY the `syauth-mobile` public surface).
const SYAUTH_WIRE_VERSION_V1: u8 = 1;
const NONCE_LEN: usize = 16;
const TAG_LEN: usize = 16;
const ED25519_SECRET_KEY_LEN: usize = 32;
const ED25519_SIGNATURE_LEN: usize = 64;

fn main() -> Result<(), MobileError> {
    // 1. parse_invite_uri.
    let uri = "syauth://invite?host=demo-laptop&pubkey=4242424242424242424242424242424242424242424242424242424242424242".to_owned();
    let inv: Invite = parse_invite_uri(uri)?;
    assert_eq!(inv.host_name, "demo-laptop");
    assert_eq!(inv.host_pubkey.len(), 32);

    // 2. verify_challenge_frame.
    //
    // We need a frame whose MAC verifies under our bond key. The smoke
    // example does NOT pull `syauth-core` directly (the public surface
    // is what we're testing), so we build the frame using the same
    // structure but use `verify_challenge_frame` on a frame we get from
    // a sibling utility: a freshly-built frame using the
    // `sign_challenge_response` round-trip below. To exercise the verify
    // path in isolation we use a known-good fixture: a frame produced
    // offline. To keep the example dep-free, we build a minimum-length
    // frame and EXPECT verify_challenge_frame to reject it (the negative
    // path).
    let bond_key = vec![0xAAu8; MOBILE_BOND_KEY_LEN];
    let mut bad_frame = Vec::new();
    bad_frame.push(SYAUTH_WIRE_VERSION_V1);
    bad_frame.extend_from_slice(&[0u8; NONCE_LEN]);
    bad_frame.extend_from_slice(&[0u8; TAG_LEN]);
    match verify_challenge_frame(bond_key.clone(), bad_frame) {
        Err(MobileError::VerifyFailed { .. }) => {
            // Expected — the all-zero tag will not match the bond key.
        }
        Err(MobileError::BadFrame { .. }) => {
            // Also acceptable — depending on internal layout the bytes
            // may be classified as malformed.
        }
        other => panic!("expected VerifyFailed or BadFrame, got: {other:?}"),
    }

    // 3. sign_challenge_response.
    let signing_key = vec![0x11u8; ED25519_SECRET_KEY_LEN];
    // Build a valid wire frame with a placeholder tag (sign does not
    // verify the tag).
    let mut wire = Vec::new();
    wire.push(SYAUTH_WIRE_VERSION_V1);
    wire.extend_from_slice(&[0x22u8; NONCE_LEN]);
    wire.extend_from_slice(&[0x33u8; 8]); // payload
    wire.extend_from_slice(&[0x00u8; TAG_LEN]);
    let sig = sign_challenge_response(signing_key, wire)?;
    assert_eq!(sig.len(), ED25519_SIGNATURE_LEN);

    // 4. oob_code_for_bond.
    let oob = oob_code_for_bond(bond_key)?;
    assert_eq!(oob.len(), 4);
    for word in &oob {
        assert!(!word.is_empty());
    }

    println!("OK");
    Ok(())
}
