//! `syauth` — top-level CLI dispatcher.
//!
//! Roadmap items S-011 (`pair`, `list`), S-013 (`install-pam`, `uninstall-pam`).
//! This binary delegates all subcommand logic to `syauth_cli::*` library
//! modules so tests and future callers can drive them in-process.
//!
//! Journeys:
//! - specs/journeys/JOURNEY-S-011-pairing-desktop.md
//! - specs/journeys/JOURNEY-S-013-pam-install-helper.md

use std::{
    io::{self, Write as _},
    process::ExitCode,
};

use anyhow::Result;
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use syauth_cli::{
    install_pam::{self, InstallOpts, InstallOutcome},
    list::run_list,
    pair::{AdapterInfo, LescOutcome, ListOpts, PairBackend, PairCandidate, PairError, PairOpts, PairingPhase, run_pair},
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
    /// Pair a phone with this desktop. Runs LE Secure Connections numeric
    /// comparison followed by the app-level 4-word OOB confirmation, then
    /// writes the bond on `[y/N]` = Y.
    Pair(PairOpts),
    /// Print the bonds file as TSV: id\tname\tstatus\tcreated_at.
    List(ListOpts),
    /// Wire `pam_syauth.so` into a PAM service file, atomically and with a
    /// `.bak` snapshot of the original.
    InstallPam(InstallOpts),
    /// Restore a PAM service file from its `.bak` and remove the bak.
    UninstallPam(UninstallOpts),
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
        Cmd::Pair(opts) => run_pair_cli(&opts).await,
        Cmd::List(opts) => run_list_cli(&opts),
        Cmd::InstallPam(opts) => run_install(&opts),
        Cmd::UninstallPam(opts) => run_uninstall(&opts),
    }
}

async fn run_pair_cli(opts: &PairOpts) -> Result<()> {
    let backend = BluerPairBackend::new(&opts.adapter);
    let phase = run_pair(opts, &backend).await?;
    let mut stdout = io::stdout().lock();
    match phase {
        PairingPhase::Bonded => Ok(()),
        other => {
            writeln!(stdout, "pair flow ended in {other:?}; no bond written")?;
            Err(anyhow::anyhow!("pair flow did not complete: {other:?}"))
        }
    }
}

fn run_list_cli(opts: &ListOpts) -> Result<()> {
    run_list(opts).map_err(Into::into)
}

/// Production [`PairBackend`] wrapping `bluer`. v0.1 surfaces every call as a
/// typed `PairError::Backend` so the binary compiles and links the dependency,
/// but the actual radio path lands with S-019 ("Full e2e on real radios"). The
/// integration test always injects a `MockPairBackend`; no production caller
/// reaches this until S-019.
struct BluerPairBackend {
    adapter_id: String,
}

impl BluerPairBackend {
    fn new(adapter_id: &str) -> Self {
        Self {
            adapter_id: adapter_id.to_owned(),
        }
    }
}

#[async_trait]
impl PairBackend for BluerPairBackend {
    async fn adapter_info(&self, _adapter_id: &str) -> Result<AdapterInfo, PairError> {
        Err(PairError::Backend {
            reason: format!("BluerPairBackend for '{}' real-radio path lands in S-019", self.adapter_id),
        })
    }
    async fn scan_peers(&self) -> Result<Vec<PairCandidate>, PairError> {
        Err(PairError::Backend {
            reason: "BluerPairBackend::scan_peers real-radio path lands in S-019".to_owned(),
        })
    }
    async fn initiate_lesc_with_peer(&self, _peer: &PairCandidate) -> Result<LescOutcome, PairError> {
        Err(PairError::Backend {
            reason: "BluerPairBackend::initiate_lesc_with_peer real-radio path lands in S-019".to_owned(),
        })
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
