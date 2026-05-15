//! `syauth-mobile` build script.
//!
//! Roadmap item S-014. Mirrors `~/sources/prrr/prrr-mobile/build.rs`:
//! `uniffi::generate_scaffolding(<udl path>).unwrap()`. The UDL file is the
//! single source of truth for the Kotlin/Swift binding surface (four
//! functions + one error enum); UniFFI parses it at compile time and
//! emits a `<name>_scaffolding.rs` file in `OUT_DIR` that `src/lib.rs`
//! pulls in via `uniffi::include_scaffolding!("mobile")`.
//!
//! The `unwrap` is the canonical pattern from prrr-mobile and the
//! UniFFI examples in the upstream repository. A failure here means the
//! UDL file is malformed (a developer-visible build break), not a
//! runtime panic — this is `build.rs`, not the cdylib.

fn main() {
    // Re-run only when the UDL changes (and the script itself). Cargo's
    // default rule (any source change) would re-run on every Rust edit.
    println!("cargo:rerun-if-changed=src/mobile.udl");
    println!("cargo:rerun-if-changed=build.rs");
    uniffi::generate_scaffolding("src/mobile.udl").expect("uniffi scaffolding generation");
}
