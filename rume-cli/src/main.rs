use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "rume", version, about = "Rume framework CLI")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
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
    /// Validate the project setup.
    Doctor,
    /// Generate a component.
    Generate { kind: String },
}

#[derive(Subcommand)]
enum EnvCommands {
    /// Inspect resolved environment and configuration.
    Check,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::New { name }) => {
            println!("scaffolding project: {name}");
        }
        Some(Commands::Dev) => {
            println!("starting development server");
        }
        Some(Commands::Test) => {
            println!("running tests");
        }
        Some(Commands::Env { command }) => match command {
            EnvCommands::Check => {
                println!("checking environment");
            }
        },
        Some(Commands::Doctor) => {
            println!("running diagnostics");
        }
        Some(Commands::Generate { kind }) => {
            println!("generating: {kind}");
        }
        None => {
            println!("Rume CLI. Run `rume --help` for usage.");
        }
    }
}
