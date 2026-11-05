#![forbid(unsafe_code)]

mod cli;
mod commands;
mod prompts;
mod ui;

use crate::cli::Cli;
use clap::Parser;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    commands::run(cli).await;
}
