//! Known-answer-test (KAT) vectors for the S-004 crypto layer.
//!
//! Reads `crates/syauth-core/testdata/kat.json` and asserts that, for every
//! pinned vector, `compute_tag` and `sign_frame` produce byte-for-byte the
//! `expected_tag` and `expected_signature` in the file. Also exercises the
//! verification path on the expected outputs.
//!
//! Hex encoding convention (matches `kat.json::_doc`):
//!   - lowercase
//!   - no `0x` prefix
//!   - no separators
//!
//! How the vectors were generated: see `JOURNEY-S-004-crypto-primitives.md`
//! §4 "How KAT vectors were generated". A `#[test] #[ignore]` helper in
//! this file (`bootstrap_print_kat_vectors`) recomputes and prints the
//! expected fields for the canonical inputs; it is *not* run in normal
//! test runs (gated by `#[ignore]`).

use serde::Deserialize;
use syauth_core::{
    frame::{Frame, NONCE_LEN, SYAUTH_WIRE_VERSION_V1, TAG_LEN},
    mac::{BOND_KEY_BYTES, compute_tag, verify_tag},
    sign::{SIGNATURE_LEN, Signature, SigningKey, sign_frame, verify_frame},
};

/// Absolute path to the KAT JSON file, resolved at compile time.
const KAT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/kat.json");

#[derive(Deserialize)]
struct KatFile {
    #[serde(default)]
    _doc: Option<String>,
    vectors: Vec<KatVector>,
}

#[derive(Deserialize)]
struct KatVector {
    name: String,
    version: u8,
    nonce: String,
    payload: String,
    bond_key: String,
    signing_key: String,
    expected_tag: String,
    expected_signature: String,
}

/// Decode a lowercase hex string into a `Vec<u8>`, with a clear panic
/// message on failure so a developer who edits the JSON sees exactly
/// which field went wrong.
fn hex_decode(field: &str, value: &str) -> Vec<u8> {
    hex::decode(value).unwrap_or_else(|e| panic!("KAT field `{field}` is not valid lowercase hex: {e}"))
}

fn hex_into_array<const N: usize>(field: &str, value: &str) -> [u8; N] {
    let bytes = hex_decode(field, value);
    assert_eq!(bytes.len(), N, "KAT field `{field}` must be {N} bytes (got {})", bytes.len());
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes);
    out
}

fn load_kat_file() -> KatFile {
    let raw = std::fs::read_to_string(KAT_PATH).unwrap_or_else(|e| panic!("failed to read {KAT_PATH}: {e}"));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("failed to parse {KAT_PATH}: {e}"))
}

fn build_frame_from_vector(v: &KatVector) -> Frame {
    assert_eq!(v.version, SYAUTH_WIRE_VERSION_V1, "vector `{}` has wrong version", v.name);
    let nonce: [u8; NONCE_LEN] = hex_into_array("nonce", &v.nonce);
    let payload = hex_decode("payload", &v.payload);
    Frame {
        version: v.version,
        nonce,
        payload,
        // The tag suffix on the parsed `Frame` is irrelevant to signing
        // and MAC'ing — both operate on `body_bytes()`. We zero it here
        // for clarity.
        tag: [0u8; TAG_LEN],
    }
}

#[test]
fn kat_file_loads_and_verifies_byte_for_byte() {
    let kat = load_kat_file();
    assert!(
        kat.vectors.len() >= 3,
        "KAT file must define at least 3 vectors, got {}",
        kat.vectors.len()
    );

    for v in &kat.vectors {
        let frame = build_frame_from_vector(v);
        let body = frame.body_bytes().expect("body_bytes");
        let bond_key: [u8; BOND_KEY_BYTES] = hex_into_array("bond_key", &v.bond_key);
        let signing_seed: [u8; 32] = hex_into_array("signing_key", &v.signing_key);
        let sk = SigningKey::from_bytes(&signing_seed);
        let pk = sk.verifying_key();

        // Tag byte-for-byte.
        let expected_tag: [u8; TAG_LEN] = hex_into_array("expected_tag", &v.expected_tag);
        let computed_tag = compute_tag(&bond_key, &body);
        assert_eq!(computed_tag, expected_tag, "vector `{}`: tag mismatch", v.name);
        assert!(
            verify_tag(&bond_key, &body, &expected_tag),
            "vector `{}`: expected tag fails verify_tag",
            v.name
        );

        // Signature byte-for-byte.
        let expected_sig_bytes: [u8; SIGNATURE_LEN] = hex_into_array("expected_signature", &v.expected_signature);
        let expected_sig = Signature::from_bytes(&expected_sig_bytes);
        let computed_sig = sign_frame(&sk, &frame).expect("sign");
        assert_eq!(
            computed_sig.to_bytes(),
            expected_sig.to_bytes(),
            "vector `{}`: signature mismatch",
            v.name
        );
        verify_frame(&pk, &frame, &expected_sig).unwrap_or_else(|e| panic!("vector `{}`: expected sig fails verify_frame: {e}", v.name));
    }
}

/// Bootstrap helper — prints the canonical KAT inputs and the freshly-
/// computed expected outputs in the `kat.json` shape. Used once at
/// authoring time to populate the JSON file. Run with:
///
/// ```ignore
/// cargo test -p syauth-core --test kat -- --ignored --nocapture bootstrap_print_kat_vectors
/// ```
///
/// Kept in tree (rather than `#[cfg(test)]`-gated and deleted post-bootstrap)
/// so any future maintainer who needs to regenerate the JSON has a documented
/// procedure on disk. The helper is `#[ignore]` so it does not run in
/// normal `cargo test` invocations.
#[test]
#[ignore]
fn bootstrap_print_kat_vectors() {
    // Canonical inputs, pinned in this file. Hex-encoded as lowercase, no
    // separators. The `bond_key` and `signing_key` are arbitrary
    // 32-byte sequences; their entropy is not load-bearing for the KAT
    // (the test pins behavior, not security).
    struct Input {
        name: &'static str,
        nonce_hex: &'static str,
        payload: Vec<u8>,
        bond_key_hex: &'static str,
        signing_key_hex: &'static str,
    }

    let inputs = [
        Input {
            name: "kat-01-empty-payload",
            nonce_hex: "00112233445566778899aabbccddeeff",
            payload: vec![],
            bond_key_hex: "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            signing_key_hex: "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20",
        },
        Input {
            name: "kat-02-typical-32b-payload",
            nonce_hex: "1010101010101010101010101010101a",
            payload: (0u8..32).collect(),
            bond_key_hex: "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f",
            signing_key_hex: "2122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f40",
        },
        Input {
            name: "kat-03-max-payload",
            nonce_hex: "ffeeddccbbaa99887766554433221100",
            payload: vec![0xAAu8; syauth_core::frame::MAX_PAYLOAD_LEN],
            bond_key_hex: "404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f",
            signing_key_hex: "4142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f60",
        },
    ];

    println!("{{");
    println!(
        "  \"_doc\": \"S-004 KAT vectors. Hex is lowercase, no 0x prefix, no separators. Regenerate via `cargo test -p syauth-core --test kat -- --ignored --nocapture bootstrap_print_kat_vectors`.\","
    );
    println!("  \"vectors\": [");
    for (i, input) in inputs.iter().enumerate() {
        let nonce_bytes: [u8; NONCE_LEN] = hex_into_array("nonce", input.nonce_hex);
        let bond_key: [u8; BOND_KEY_BYTES] = hex_into_array("bond_key", input.bond_key_hex);
        let signing_seed: [u8; 32] = hex_into_array("signing_key", input.signing_key_hex);
        let sk = SigningKey::from_bytes(&signing_seed);
        let frame = Frame {
            version: SYAUTH_WIRE_VERSION_V1,
            nonce: nonce_bytes,
            payload: input.payload.clone(),
            tag: [0u8; TAG_LEN],
        };
        let body = frame.body_bytes().expect("body_bytes");
        let tag = compute_tag(&bond_key, &body);
        let sig = sign_frame(&sk, &frame).expect("sign");

        let comma = if i + 1 < inputs.len() { "," } else { "" };
        println!("    {{");
        println!("      \"name\": \"{}\",", input.name);
        println!("      \"version\": {},", SYAUTH_WIRE_VERSION_V1);
        println!("      \"nonce\": \"{}\",", input.nonce_hex);
        println!("      \"payload\": \"{}\",", hex::encode(&input.payload));
        println!("      \"bond_key\": \"{}\",", input.bond_key_hex);
        println!("      \"signing_key\": \"{}\",", input.signing_key_hex);
        println!("      \"expected_tag\": \"{}\",", hex::encode(tag));
        println!("      \"expected_signature\": \"{}\"", hex::encode(sig.to_bytes()));
        println!("    }}{}", comma);
    }
    println!("  ]");
    println!("}}");
}
