//! `syauth` — top-level CLI dispatcher.
//!
//! Roadmap items S-011 (`pair`), S-012 (`list`/`revoke`/`status`), and
//! S-013 (`install-pam`/`uninstall-pam`). This binary delegates all
//! subcommand logic to `syauth_cli::*` library modules so tests and
//! future callers can drive them in-process.
//!
//! Journey: specs/journeys/JOURNEY-S-013-pam-install-helper.md

use std::{
    io::{self, Write as _},
    process::ExitCode,
};

use anyhow::Result;
use clap::{Parser, Subcommand};
use syauth_cli::{
    install_pam::{self, InstallOpts, InstallOutcome},
    uninstall_pam::{self, UninstallOpts, UninstallOutcome},
};

#[derive(Debug, Parser)]
#[command(
    name = "syauth",
    version,
    about = "Phone-as-key unlock for Linux PAM",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Wire `pam_syauth.so` into a PAM service file, atomically and with a
    /// `.bak` snapshot of the original.
    InstallPam(InstallOpts),
    /// Restore a PAM service file from its `.bak` and remove the bak.
    UninstallPam(UninstallOpts),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let res = dispatch(cli);
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let mut stderr = io::stderr().lock();
            // Print the full error chain so users see the root cause.
            let _ = writeln!(stderr, "error: {err}");
            let mut src = err.source();
            while let Some(s) = src {
                let _ = writeln!(stderr, "  caused by: {s}");
                src = s.source();
            }
            ExitCode::FAILURE
        }
    }
}

fn dispatch(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::InstallPam(opts) => run_install(&opts),
        Cmd::UninstallPam(opts) => run_uninstall(&opts),
    }
}

fn run_install(opts: &InstallOpts) -> Result<()> {
    let outcome = install_pam::install(opts)?;
    let mut stdout = io::stdout().lock();
    match outcome {
        InstallOutcome::AlreadyInstalled { path } => {
            writeln!(stdout, "syauth line already present in {}; no changes", path.display())?;
        }
        InstallOutcome::Installed { service, backup } => {
            writeln!(
                stdout,
                "wrote backup to {}; inserted syauth at top of auth block in {}",
                backup.display(),
                service.display()
            )?;
        }
    }
    Ok(())
}

fn run_uninstall(opts: &UninstallOpts) -> Result<()> {
    let outcome = uninstall_pam::uninstall(opts)?;
    match outcome {
        UninstallOutcome::NotInstalled { path } => {
            let mut stderr = io::stderr().lock();
            writeln!(stderr, "warning: no syauth line found in {}; nothing to uninstall", path.display())?;
        }
        UninstallOutcome::Restored { service, backup } => {
            let mut stdout = io::stdout().lock();
            writeln!(stdout, "restored {} from backup; removed {}", service.display(), backup.display())?;
        }
    }
    Ok(())
}
