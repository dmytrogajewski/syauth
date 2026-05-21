//! `syauth` — top-level CLI dispatcher.
//!
//! Roadmap items S-011 (`pair`, `list`), S-012 (`revoke`, `status`),
//! S-013 (`install-pam`, `uninstall-pam`).
//! This binary delegates all subcommand logic to `syauth_cli::*` library
//! modules so tests and future callers can drive them in-process.
//!
//! Journeys:
//! - specs/journeys/JOURNEY-S-011-pairing-desktop.md
//! - specs/journeys/JOURNEY-S-012-day2-cli.md
//! - specs/journeys/JOURNEY-S-013-pam-install-helper.md

use std::{
    io::{self, Write as _},
    process::ExitCode,
};

use anyhow::Result;
use clap::{Parser, Subcommand};
use syauth_cli::{
    doctor::{DoctorOpts, run_doctor},
    install_pam::{self, InstallOpts, InstallOutcome},
    install_presenced::{self, InstallPresencedOpts, InstallPresencedOutcome},
    list::run_list,
    pair::ListOpts,
    revoke::{RevokeOpts, run_revoke},
    status::{StatusOpts, run_status},
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
    /// Print the bonds file as TSV: id\tname\tstatus\tcreated_at.
    List(ListOpts),
    /// Mark a bond as revoked (idempotent). The bond record itself is
    /// preserved so the audit trail survives; the PAM module refuses
    /// unlock attempts from revoked peers.
    Revoke(RevokeOpts),
    /// Print adapter state, advertising state, bond count, and the most
    /// recent unlock outcome. Read-only — never writes to the host.
    Status(StatusOpts),
    /// Wire `pam_syauth.so` into a PAM service file, atomically and with a
    /// `.bak` snapshot of the original.
    InstallPam(InstallOpts),
    /// Restore a PAM service file from its `.bak` and remove the bak.
    UninstallPam(UninstallOpts),
    /// Install the `syauth-presenced` systemd user unit and (live mode)
    /// reload + enable + start it.
    InstallPresenced(InstallPresencedOpts),
    /// Inspect the unlock chain: daemon liveness, bonds file, keys file
    /// modes, BlueZ adapter, systemd user unit state, audit-log tail,
    /// and the `XDG_RUNTIME_DIR` SSH-session caveat. Emits one
    /// greppable `key=value` line per probe plus a final
    /// `doctor=ok|warn|fail` summary. `--json` emits the same data as
    /// a typed JSON object for tooling.
    Doctor(DoctorOpts),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(err) => {
            let mut stderr = io::stderr().lock();
            let _ = writeln!(stderr, "error: failed to start tokio runtime: {err}");
            return ExitCode::FAILURE;
        }
    };
    let res = runtime.block_on(async { dispatch(cli).await });
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let mut stderr = io::stderr().lock();
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

async fn dispatch(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::List(opts) => run_list_cli(&opts),
        Cmd::Revoke(opts) => run_revoke_cli(&opts),
        Cmd::Status(opts) => run_status_cli(&opts).await,
        Cmd::InstallPam(opts) => run_install(&opts),
        Cmd::UninstallPam(opts) => run_uninstall(&opts),
        Cmd::InstallPresenced(opts) => run_install_presenced(&opts),
        Cmd::Doctor(opts) => run_doctor_cli(&opts),
    }
}

fn run_doctor_cli(opts: &DoctorOpts) -> Result<()> {
    run_doctor(opts).map_err(Into::into)
}

fn run_revoke_cli(opts: &RevokeOpts) -> Result<()> {
    run_revoke(opts).map_err(Into::into)
}

async fn run_status_cli(opts: &StatusOpts) -> Result<()> {
    run_status(opts).await.map_err(Into::into)
}

fn run_list_cli(opts: &ListOpts) -> Result<()> {
    run_list(opts).map_err(Into::into)
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
    if opts.with_presenced {
        let presenced_opts = InstallPresencedOpts {
            from: opts.presenced_from.clone(),
            unit_dir: opts.presenced_unit_dir.clone(),
            dry_run: opts.presenced_dry_run,
        };
        let presenced_outcome = install_presenced::install_presenced(&presenced_opts, &mut stdout)?;
        report_install_presenced(&mut stdout, &presenced_outcome)?;
    }
    Ok(())
}

fn run_install_presenced(opts: &InstallPresencedOpts) -> Result<()> {
    let mut stdout = io::stdout().lock();
    let outcome = install_presenced::install_presenced(opts, &mut stdout)?;
    report_install_presenced(&mut stdout, &outcome)?;
    Ok(())
}

fn report_install_presenced(stdout: &mut io::StdoutLock<'_>, outcome: &InstallPresencedOutcome) -> Result<()> {
    match outcome {
        InstallPresencedOutcome::Installed { unit_path, binary_path } => {
            writeln!(
                stdout,
                "installed syauth-presenced: wrote unit {}, copied binary to {}",
                unit_path.display(),
                binary_path.display()
            )?;
        }
        InstallPresencedOutcome::DryRun { unit_path, source_binary } => {
            writeln!(
                stdout,
                "dry-run: wrote unit {} pointing at {}",
                unit_path.display(),
                source_binary.display()
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
