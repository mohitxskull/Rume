mod codegen;
mod config;
mod dev;
mod doctor;
mod env;
mod generate;
mod new;
mod test;

use crate::cli::{Cli, Commands};
use std::sync::atomic::{AtomicBool, Ordering};

static NO_COLOR: AtomicBool = AtomicBool::new(false);
static VERBOSE: AtomicBool = AtomicBool::new(false);

pub fn is_no_color() -> bool {
    NO_COLOR.load(Ordering::Relaxed)
}

pub fn is_verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

pub fn run(cli: Cli) {
    NO_COLOR.store(cli.no_color, Ordering::Relaxed);
    VERBOSE.store(cli.verbose, Ordering::Relaxed);

    match cli.command {
        Some(Commands::New { name }) => {
            let name = match name {
                Some(n) => n,
                None => crate::prompts::ask("project name"),
            };
            if name.is_empty() {
                eprintln!("error: project name is required");
                std::process::exit(1);
            }
            new::run(&name);
        }
        Some(Commands::Dev) => dev::run(),
        Some(Commands::Test) => test::run(),
        Some(Commands::Env { command }) => env::run(command),
        Some(Commands::Config { command }) => config::run(command),
        Some(Commands::Doctor) => doctor::run(),
        Some(Commands::Codegen) => codegen::run(),
        Some(Commands::Generate { kind, name }) => generate::run(kind.as_deref(), name.as_deref()),
        Some(Commands::Completions { shell }) => {
            use clap::CommandFactory;
            use clap_complete::generate;
            let mut cmd = Cli::command();
            generate(shell, &mut cmd, "rume", &mut std::io::stdout());
        }
        Some(Commands::List) => {
            println!("Available commands:");
            println!("  new <name>          Scaffold a new Rume project");
            println!("  dev                 Run the application in development mode");
            println!("  test                Run the test suite");
            println!("  env check [--all]   Inspect resolved environment");
            println!("  config check        Inspect merged configuration");
            println!("  doctor              Validate the project setup");
            println!(
                "  generate <kind>     Generate a component (handler, service, model, middleware)"
            );
            println!("  codegen             Regenerate barrel files");
            println!("  completions <sh>    Generate shell completions");
            println!("  list                List all available commands");
            println!("  shell               Start an interactive shell");
        }
        Some(Commands::Shell) => {
            use clap::Parser;
            use std::io::{self, BufRead, Write};
            let stdin = io::stdin();
            let mut stdout = io::stdout();
            loop {
                let _ = write!(stdout, "rume> ");
                let _ = stdout.flush();
                let mut line = String::new();
                if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if line == "exit" || line == "quit" {
                    break;
                }
                let args: Vec<&str> = line.split_whitespace().collect();
                let cli = match Cli::try_parse_from(
                    std::iter::once("rume").chain(args.iter().copied()),
                ) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("{}", e);
                        continue;
                    }
                };
                let saved_no_color = is_no_color();
                let saved_verbose = is_verbose();
                NO_COLOR.store(cli.no_color, Ordering::Relaxed);
                VERBOSE.store(cli.verbose, Ordering::Relaxed);
                match cli.command {
                    Some(Commands::New { name }) => {
                        let name = name.unwrap_or_else(|| crate::prompts::ask("project name"));
                        if !name.is_empty() {
                            new::run(&name);
                        }
                    }
                    Some(Commands::Generate { kind, name }) => {
                        generate::run(kind.as_deref(), name.as_deref());
                    }
                    Some(Commands::List) => {
                        println!(
                            "Available commands: new, dev, test, env, config, doctor, generate, codegen, completions, list, shell"
                        );
                    }
                    Some(other) => {
                        eprintln!("Command not yet supported in shell mode. Use the CLI directly.");
                        let _ = other;
                    }
                    None => {}
                }
                NO_COLOR.store(saved_no_color, Ordering::Relaxed);
                VERBOSE.store(saved_verbose, Ordering::Relaxed);
            }
        }
        None => {
            println!("Rume CLI. Run `rume --help` for usage.");
        }
    }
}
