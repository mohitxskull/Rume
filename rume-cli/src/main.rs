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
    /// Inspect resolved environment and configuration.
    EnvCheck,
    /// Validate the project setup.
    Doctor,
    /// Generate a component.
    Generate { kind: String },
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
        Some(Commands::EnvCheck) => {
            println!("checking environment");
        }
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
