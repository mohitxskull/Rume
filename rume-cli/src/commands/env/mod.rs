mod check;
mod explain;

use crate::cli::EnvCommands;

pub fn run(command: EnvCommands) {
    match command {
        EnvCommands::Check {
            all,
            unredacted,
            format,
            json,
        } => {
            let fmt = if json { "json" } else { &format };
            check::run(all, unredacted, fmt);
        }
        EnvCommands::Explain {
            key,
            unredacted,
            json,
        } => explain::run(&key, unredacted, json),
    }
}
