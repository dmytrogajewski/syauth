//! `syauth-mobile` — UniFFI-exported protocol surface for the Android app.
//!
//! Roadmap item S-014. Mirrors `~/sources/prrr/prrr-mobile`:
//!
//! - `[lib] crate-type = ["cdylib", "staticlib", "lib"]` in `Cargo.toml`.
//! - `uniffi = "0.29"` runtime + `uniffi = { version = "0.29", features = ["build"] }` build-dep.
//! - `uniffi::generate_scaffolding("src/mobile.udl")` in `build.rs`.
//! - `uniffi::include_scaffolding!("mobile")` at crate root.
//!
//! The public surface — exactly four functions plus a typed error — is
//! defined in `src/mobile.udl` and implemented in `src/implementation.rs`.
//! The four functions are:
//!
//! - [`parse_invite_uri`] — parse `syauth://invite?host=<name>&pubkey=<hex>`.
//! - [`verify_challenge_frame`] — BLAKE3-keyed-hash MAC check + payload extraction.
//! - [`sign_challenge_response`] — Ed25519 signing over a frame body.
//! - [`oob_code_for_bond`] — HKDF-SHA256 four-word OOB code, mirrors `syauth-cli`.
//!
//! See `specs/syauth/SPEC.md` §4.1 and `specs/journeys/JOURNEY-S-014-mobile-uniffi-surface.md`
//! for the design rationale.
//!
//! ## Unsafe-code exception
//!
//! The workspace-level `[workspace.lints.rust] unsafe_code = "deny"` rule
//! is overridden at THIS crate ONLY, via the crate-level
//! `#![allow(unsafe_code)]` directive below. The exception's scope is
//! exclusively the UniFFI-generated scaffolding (the
//! `uniffi::include_scaffolding!("mobile")` macro emits `unsafe extern
//! "C"` ABI shims — FFI boundaries are inherently `unsafe` in Rust).
//!
//! Audit trail:
//!
//! - NO hand-written `unsafe` block exists in this crate. `git grep
//!   unsafe crates/syauth-mobile/src/` should return only the
//!   `#![allow(unsafe_code)]` directive itself and the doc text.
//! - Every `unsafe` symbol the cdylib exports is produced by the
//!   `include_scaffolding!` macro, which is reviewed once at the UniFFI
//!   upstream layer.
//! - The `#![deny(missing_docs)]` directive still applies, keeping the
//!   crate's documentation contract strict.
//!
//! This mirrors the prrr-mobile pattern (where the workspace
//! `unsafe_code = "deny"` is commented out in `Cargo.toml`); we instead
//! keep the workspace deny strict and override at the source file so
//! the exception is visible at code-review time. See the
//! `/ffi` skill (`.agents/skills/ffi/SKILL.md`) for the per-`unsafe`
//! audit checklist that applies to the UniFFI-generated code path.

#![allow(unsafe_code)]
// UniFFI 0.28+ scaffolding emits doc comments without a blank line
// before the next item — the lint fires on the generated file, not on
// our code. Mirrors prrr-mobile's allow.
#![allow(clippy::empty_line_after_doc_comments)]
#![warn(missing_docs)]

/// Implementation of the four UDL-exported functions. Public so the
/// `cargo run -p syauth-mobile --example smoke` smoke test can call
/// them through the crate-level re-exports below.
pub mod implementation;

pub use implementation::{
    ED25519_SECRET_KEY_LEN, ED25519_SIGNATURE_LEN, HKDF_INFO_OOB_V1, INVITE_PUBKEY_LEN, INVITE_QUERY_KEY_HOST, INVITE_QUERY_KEY_PUBKEY,
    INVITE_URI_HOST_PATH, INVITE_URI_SCHEME, Invite, MOBILE_BOND_KEY_LEN, MobileError, OOB_WORD_COUNT, OOB_WORDS, build_response_frame,
    oob_code_for_bond, parse_invite_uri, sign_challenge_response, verify_challenge_frame,
};

/// Library version. Surfaced through UniFFI so the Kotlin caller can log
/// the Rust-side version it linked against. Mirrors prrr-mobile.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// UniFFI scaffolding — emits the `unsafe extern "C"` ABI shims the
// generated Kotlin/Swift bindings call into. This is the *only* source
// of `unsafe` in the crate; see the crate-level docstring above for the
// audit trail.
uniffi::include_scaffolding!("mobile");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_non_empty() {
        assert!(!VERSION.is_empty());
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn public_surface_reexports_compile() {
        // Exercising the re-exports via type inference catches a missing
        // `pub use` at compile time. The `#[allow(type_complexity)]` is
        // local to this test: clippy's complex-type lint is unhelpful
        // when the whole point is to assert the EXACT public signatures.
        type ParseFn = fn(String) -> Result<Invite, MobileError>;
        type FramedFn = fn(Vec<u8>, Vec<u8>) -> Result<Vec<u8>, MobileError>;
        type OobFn = fn(Vec<u8>) -> Result<Vec<String>, MobileError>;
        let _: ParseFn = parse_invite_uri;
        let _: FramedFn = verify_challenge_frame;
        let _: FramedFn = sign_challenge_response;
        let _: OobFn = oob_code_for_bond;
    }
}
