//! S-010 smoke test for the `BlueZBtPeer` real-radio path.
//!
//! Journey: specs/journeys/JOURNEY-S-010-bluez-transport.md
//!
//! This test is **gated**. It is a no-op (with an explanatory skip message)
//! unless the environment variable `SYAUTH_E2E=1` is set. The smoke test:
//!
//! 1. attempts a `bluer::Session::new()` to open the system DBus;
//! 2. asks for the adapter named by the env var `SYAUTH_TEST_ADAPTER`
//!    (default `hci0`);
//! 3. reports the adapter's `is_powered()` state via println so the CI log
//!    captures the result;
//! 4. asserts the round-trip never panicked (`AdapterMissing` or a typed
//!    `Backend` failure is acceptable on machines without a live BlueZ
//!    daemon — the smoke test only proves the code path *links* against
//!    `bluer` correctly).
//!
//! Environment requirements when the gate is on:
//!
//! - Linux host with `bluez` and a session/system DBus daemon reachable.
//! - For a hermetic CI run, the BlueZ project ships `btvirt` (from
//!   `bluez-tools`); the recommended CI container exposes one virtual
//!   controller as `hci0`.
//! - Root is NOT required for the read-only smoke checks here — the system
//!   DBus exposes adapter properties to any uid by default.
//!
//! When the gate is off, `make test` stays green on a vanilla developer box
//! without bluetooth hardware.

use std::env;

/// Environment switch that enables the e2e test. Matches `tests/pam_smoke.rs`
/// so an operator setting `SYAUTH_E2E=1` runs both at once.
const E2E_GATE_VAR: &str = "SYAUTH_E2E";

/// Expected env-var value to enable the gate. Anything else (including
/// unset, `0`, or empty) skips.
const E2E_GATE_ON: &str = "1";

/// Environment variable that lets a CI runner pick a non-default adapter
/// name without recompiling. Defaults to the SPEC §4.1 default `hci0`.
const ADAPTER_ENV_VAR: &str = "SYAUTH_TEST_ADAPTER";

/// Default adapter name probed by the smoke test when the env var above is
/// unset. Matches `syauth_transport::DEFAULT_ADAPTER_NAME`.
const DEFAULT_ADAPTER: &str = "hci0";

/// Banner the test prints when it skips. Captured by `cargo test`'s stdout
/// when run with `-- --nocapture` so a CI maintainer can grep for it.
const SKIP_BANNER: &str = "bluer_smoke: skipped (SYAUTH_E2E != 1)";

#[tokio::test]
async fn bluer_smoke() {
    if env::var(E2E_GATE_VAR).ok().as_deref() != Some(E2E_GATE_ON) {
        println!("{SKIP_BANNER}");
        return;
    }

    let adapter_name = env::var(ADAPTER_ENV_VAR).unwrap_or_else(|_| DEFAULT_ADAPTER.to_owned());

    let session = match bluer::Session::new().await {
        Ok(s) => s,
        Err(err) => {
            // Acceptable failure: the host has no live system DBus. Report
            // and exit. The test still "passes" because the failure is
            // environmental, not a defect in our code.
            println!("bluer_smoke: bluer::Session::new() failed: {err}");
            return;
        }
    };

    match session.adapter(&adapter_name) {
        Ok(adapter) => match adapter.is_powered().await {
            Ok(powered) => println!("bluer_smoke: adapter '{adapter_name}' is_powered = {powered}"),
            Err(err) => println!("bluer_smoke: adapter '{adapter_name}' is_powered() failed: {err}"),
        },
        Err(err) => println!("bluer_smoke: adapter '{adapter_name}' not openable: {err}"),
    }
}
