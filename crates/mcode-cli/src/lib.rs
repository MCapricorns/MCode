//! Provides the fail-closed MCode product command skeleton.
//!
//! `run` and `resume` retain their command-line shape, but no Provider or
//! Session product service is assembled in this crate. After successful clap
//! parsing, both commands return the same setup error without reading the
//! working directory, process environment, credentials, persisted state, or
//! network.
//!
//! Exit code `1` reports the unavailable product setup. Clap keeps exit code
//! `2` for usage errors.

pub mod cli;

use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::Parser;

pub use cli::{Cli, Command};

const SETUP_ERROR: &str =
    "product commands are unavailable: first-party features are not yet delivered in this build";

/// Parses the command line and returns the product setup status.
///
/// Clap reports usage errors before this function reaches product setup.
pub fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mcode: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Rejects a parsed product command until signed Packs are active.
///
/// The parsed values are deliberately not inspected so setup failure has no
/// working-directory, filesystem, network, environment, authentication, or
/// persisted-state side effects.
///
/// # Errors
///
/// Always returns deterministic installation and activation guidance for the
/// Providers and Session Managers and their signed Packs.
pub fn run(_cli: Cli) -> Result<()> {
    bail!(SETUP_ERROR)
}
