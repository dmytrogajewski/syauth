// Journey: specs/journeys/JOURNEY-S-001-workspace-bootstrap.md
//
// Single workspace smoke test required by ROADMAP item S-001. Proves that the
// integration-test harness compiles and runs against the workspace package, so
// every later roadmap item has a working `cargo test` baseline to extend.

/// Expected output of the canonical sanity-check sum, expressed as a named
/// constant per the AGENTS.md micro-TDD rules ("named constants over literals").
const EXPECTED_SUM: u32 = 2;

#[test]
fn workspace_test_harness_compiles_and_runs() {
    let lhs: u32 = 1;
    let rhs: u32 = 1;
    assert_eq!(lhs + rhs, EXPECTED_SUM);
}
