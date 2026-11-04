mod codegen;
mod config;
mod db;
mod doctor;
mod env;
mod generate;
mod list_routes;
mod new;

use crate::cli::{Cli, Commands};
pub fn require_rume_project() {
    if !std::path::Path::new("config/app.toml").is_file() {
        eprintln!("error: not a Rume project (config/app.toml not found)");
        eprintln!("hint: run `rume new <name>` to create one, or cd into a Rume project");
        std::process::exit(1);
    }
}

pub async fn run(cli: Cli) {
    if cli.no_color {
        console::set_colors_enabled(false);
    }

    match cli.command {
        Some(Commands::New {
            name,
            path,
            version,
        }) => {
            let name = match name {
                Some(n) => n,
                None => crate::prompts::ask("project name"),
            };
            if name.is_empty() {
                eprintln!("error: project name is required");
                std::process::exit(1);
            }
            new::run(&name, path.as_deref(), version.as_deref());
        }
        Some(Commands::Env { command }) => {
            require_rume_project();
            env::run(command);
        }
        Some(Commands::Config { command }) => {
            require_rume_project();
            config::run(command);
        }
        Some(Commands::Doctor { format, json }) => {
            require_rume_project();
            let fmt = if json { "json" } else { &format };
            doctor::run(fmt);
        }
        Some(Commands::ListRoutes { format, json }) => {
            require_rume_project();
            let is_json = json || format == "json";
            list_routes::run(is_json);
        }
        Some(Commands::Codegen { dry_run }) => {
            require_rume_project();
            codegen::run(dry_run);
        }
        Some(Commands::Generate { kind, name }) => {
            require_rume_project();
            generate::run(kind.as_deref(), name.as_deref());
        }
        Some(Commands::MakeMigration { name }) => {
            require_rume_project();
            db::make_migration(name);
        }
        Some(Commands::Db { command }) => {
            require_rume_project();
            db::run(command).await;
        }
        Some(Commands::DbStatus) => {
            require_rume_project();
            db::run(crate::cli::DbCommands::Status).await;
        }
        Some(Commands::DbMigrate) => {
            require_rume_project();
            db::run(crate::cli::DbCommands::Migrate).await;
        }
        Some(Commands::DbRollback) => {
            require_rume_project();
            db::run(crate::cli::DbCommands::Rollback).await;
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
