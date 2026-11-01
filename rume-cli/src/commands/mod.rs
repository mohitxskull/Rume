mod codegen;
mod config;
mod doctor;
mod env;
mod generate;
mod new;

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

pub fn require_rume_project() {
    if !std::path::Path::new("config/app.toml").is_file() {
        eprintln!("error: not a Rume project (config/app.toml not found)");
        eprintln!("hint: run `rume new <name>` to create one, or cd into a Rume project");
        std::process::exit(1);
    }
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
        Some(Commands::Env { command }) => {
            require_rume_project();
            env::run(command);
        }
        Some(Commands::Config { command }) => {
            require_rume_project();
            config::run(command);
        }
        Some(Commands::Doctor { format }) => {
            require_rume_project();
            doctor::run(&format);
        }
        Some(Commands::Codegen) => {
            require_rume_project();
            codegen::run();
        }
        Some(Commands::Generate { kind, name }) => {
            require_rume_project();
            generate::run(kind.as_deref(), name.as_deref());
        }
        Some(Commands::Completions { shell }) => {
            use clap::CommandFactory;
            use clap_complete::generate;
            let mut cmd = Cli::command();
            generate(shell, &mut cmd, "rume", &mut std::io::stdout());
        }
        None => {
            println!("Rume CLI. Run `rume --help` for usage.");
        }
    }
}
