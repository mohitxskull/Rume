mod check;
mod explain;

use crate::cli::ConfigCommands;

pub fn run(command: ConfigCommands) {
    match command {
        ConfigCommands::Check {
            unredacted,
            format,
            json,
        } => {
            let fmt = if json { "json" } else { &format };
            check::run(unredacted, fmt);
        }
        ConfigCommands::Explain { key, json } => explain::run(&key, json),
    }
}
