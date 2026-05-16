//! `syauth-cli` library surface.
//!
//! The binary in `src/main.rs` is a thin clap-based dispatcher; every
//! subcommand's logic lives here as a library function so that integration
//! tests, future fuzz harnesses, and in-process callers can drive the
//! behavior directly. The canonical user paths remain the built `syauth`
//! binary driven by `assert_cmd` in `tests/install_pam.rs` and the in-process
//! mock-backend test in `tests/pair_flow.rs`.
//!
//! Roadmap: specs/syauth/ROADMAP.md items S-011, S-012, S-013.
//! Journeys:
//! - specs/journeys/JOURNEY-S-011-pairing-desktop.md
//! - specs/journeys/JOURNEY-S-012-day2-cli.md
//! - specs/journeys/JOURNEY-S-013-pam-install-helper.md

pub mod install_pam;
pub mod list;
pub mod oob;
pub mod pair;
pub mod provision;
pub mod revoke;
pub mod status;
pub mod uninstall_pam;
