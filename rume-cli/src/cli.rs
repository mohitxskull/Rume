use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "rume", version, about = "Rume framework CLI")]
pub struct Cli {
    /// Disable colored output.
    #[arg(long, global = true)]
    pub no_color: bool,
    /// Enable verbose output.
    #[arg(long, global = true)]
    pub verbose: bool,
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Scaffold a new Rume project.
    New {
        /// Project name.
        name: Option<String>,
        /// Path to local Rume framework crate (for local development or testing).
        #[arg(long)]
        path: Option<String>,
        /// Rume version to depend on (defaults to CLI version).
        #[arg(long)]
        version: Option<String>,
    },
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
    Doctor {
        /// Output format: human (default) or json.
        #[arg(long, default_value = "human")]
        format: String,
    },
    /// List registered routes.
    #[command(name = "list:routes", alias = "list-routes")]
    ListRoutes {
        /// Output routes as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Generate shell completions.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Regenerate barrel and registration files.
    Codegen,
    /// Generate a component.
    Generate {
        kind: Option<String>,
        name: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum EnvCommands {
    /// Inspect resolved environment and configuration.
    Check {
        /// Include variables inherited from the process environment.
        #[arg(long)]
        all: bool,
        /// Show secret values in plaintext (default: redacted).
        #[arg(long)]
        unredacted: bool,
        /// Output format: human (default) or json.
        #[arg(long, default_value = "human")]
        format: String,
    },
    /// Explain how a single environment variable was resolved.
    Explain {
        /// Environment variable name to explain.
        key: String,
    },
}

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Print the merged configuration.
    Check {
        /// Show secret values in plaintext (default: redacted).
        #[arg(long)]
        unredacted: bool,
        /// Output format: human (default) or json.
        #[arg(long, default_value = "human")]
        format: String,
    },
}
