use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "rume", version, about = "Rume framework CLI")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Scaffold a new Rume project.
    New { name: String },
    /// Run the application in development mode.
    Dev,
    /// Run the test suite.
    Test,
    /// Inspect and validate environment/configuration.
    Env {
        #[command(subcommand)]
        command: EnvCommands,
    },
    /// Inspect and validate configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
    /// Validate the project setup.
    Doctor,
    /// Generate a component.
    Generate { kind: String },
}

#[derive(Subcommand)]
pub enum EnvCommands {
    /// Inspect resolved environment and configuration.
    Check {
        /// Include variables inherited from the process environment.
        #[arg(long)]
        all: bool,
    },
}

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Print the merged configuration.
    Check,
}
