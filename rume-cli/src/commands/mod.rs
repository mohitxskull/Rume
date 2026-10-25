mod config;
mod dev;
mod doctor;
mod env;
mod generate;
mod new;
mod test;

use crate::cli::{Cli, Commands};

pub fn run(cli: Cli) {
    match cli.command {
        Some(Commands::New { name }) => new::run(&name),
        Some(Commands::Dev) => dev::run(),
        Some(Commands::Test) => test::run(),
        Some(Commands::Env { command }) => env::run(command),
        Some(Commands::Config { command }) => config::run(command),
        Some(Commands::Doctor) => doctor::run(),
        Some(Commands::Generate { kind }) => generate::run(&kind),
        None => {
            println!("Rume CLI. Run `rume --help` for usage.");
        }
    }
}
