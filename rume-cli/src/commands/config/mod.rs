mod check;

use crate::cli::ConfigCommands;

pub fn run(command: ConfigCommands) {
    match command {
        ConfigCommands::Check => check::run(),
    }
}
