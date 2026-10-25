mod check;

use crate::cli::EnvCommands;

pub fn run(command: EnvCommands) {
    match command {
        EnvCommands::Check => check::run(),
    }
}
