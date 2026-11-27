//! Build automation and maintenance tasks for the Rume workspace.
//!
//! Run via `cargo xtask <subcommand>` (aliased in `.cargo/config.toml`).

#![forbid(unsafe_code)]

pub mod commands;
pub mod util;

use clap::{Parser, Subcommand};
use std::env;

use crate::commands::{
    audit::run_audit, ci::run_ci, doc::run_doc, features::run_features, quick::run_quick,
    test::run_test,
};
use crate::util::workspace_root;

/// Command-line interface definition for the `xtask` binary.
#[derive(Parser)]
#[command(name = "xtask")]
#[command(about = "Workspace automation task runner for Rume", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

/// Enumeration of available automation tasks and subcommands.
#[derive(Subcommand)]
enum Commands {
    /// Full pre-commit / CI verification pipeline
    Ci {
        /// Treat missing optional tools (cargo-audit) as fatal errors
        #[arg(long)]
        strict: bool,
        /// Include Docker container integration tests (requires running Docker daemon)
        #[arg(long)]
        containers: bool,
    },

    /// Fast inner loop: format, clippy, and core unit tests
    Quick,

    /// Run workspace unit and integration tests
    Test {
        /// Include Docker container integration tests (requires running Docker daemon)
        #[arg(long)]
        containers: bool,
    },

    /// Verify isolated backend and optional feature combinations for `rume`
    Features,

    /// Run workspace doc-tests and build rustdoc documentation
    Doc,

    /// Run security advisory and dependency policy checks via cargo-audit and cargo-deny
    Audit {
        /// Treat missing cargo-audit or cargo-deny binary as fatal error
        #[arg(long)]
        strict: bool,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let root = workspace_root()?;
    env::set_current_dir(&root)?;

    match cli.command.unwrap_or(Commands::Ci {
        strict: false,
        containers: false,
    }) {
        Commands::Ci { strict, containers } => run_ci(strict, containers)?,
        Commands::Quick => run_quick()?,
        Commands::Test { containers } => run_test(containers)?,
        Commands::Features => run_features()?,
        Commands::Doc => run_doc()?,
        Commands::Audit { strict } => run_audit(strict)?,
    }

    Ok(())
}
