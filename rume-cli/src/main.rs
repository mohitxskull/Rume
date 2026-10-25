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
enum EnvCommands {
    /// Inspect resolved environment and configuration.
    Check,
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Print the merged configuration.
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
                let env_name = rume::env::resolve_env_name();
                println!("environment: {env_name}");

                match rume::env::ResolvedEnv::load() {
                    Ok(resolved) => {
                        if resolved.values.is_empty() {
                            println!("(no environment variables resolved)");
                        } else {
                            println!("resolved environment:");
                            for (key, value) in &resolved.values {
                                let source = resolved
                                    .sources
                                    .get(key)
                                    .map(String::as_str)
                                    .unwrap_or("unknown");
                                let display = rume::env::redact(key, value);
                                println!("  {key}={display}  ({source})");
                            }
                        }
                    }
                    Err(err) => {
                        eprintln!("error: {err}");
                        std::process::exit(1);
                    }
                }
            }
        },
        Some(Commands::Config { command }) => match command {
            ConfigCommands::Check => {
                match rume::config::load_raw_pretty(std::path::Path::new(".")) {
                    Ok(text) => println!("{text}"),
                    Err(err) => {
                        eprintln!("error: {err}");
                        std::process::exit(1);
                    }
                }
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
