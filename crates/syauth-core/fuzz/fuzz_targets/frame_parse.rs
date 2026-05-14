// syauth-core / S-002 — libFuzzer harness for `Frame::decode`.
//
// The fuzz target asserts the decoder is **total**: every input either parses
// into a `Frame` or returns one of the three documented `FrameError` variants.
// A panic, abort, or out-of-bounds access fails the run.
//
// Run with:
//   cargo +nightly fuzz run frame_parse -- -runs=10000
// from `crates/syauth-core/`. The harness is excluded from the main workspace
// so the libFuzzer instrumentation does not leak into ordinary builds.

#![no_main]

use libfuzzer_sys::fuzz_target;
use syauth_core::Frame;

fuzz_target!(|data: &[u8]| {
    // Discard the result — the only assertion we make is that decode does
    // not panic. Any non-panic return is success for this target.
    let _ = Frame::decode(data);
});
