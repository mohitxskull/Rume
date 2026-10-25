mod cli;
mod commands;

use crate::cli::Cli;
use clap::Parser;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    commands::run(cli);
}
