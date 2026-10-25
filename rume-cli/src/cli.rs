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
    New { name: Option<String> },
    /// List all available commands.
    List,
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
    /// Generate shell completions.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Regenerate barrel and registration files.
    Codegen,
    /// Start an interactive shell (REPL).
    Shell,
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
    },
}

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Print the merged configuration.
    Check,
}
