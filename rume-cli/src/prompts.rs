//! Interactive prompt helpers using `inquire`.

use std::io::IsTerminal;
use inquire::InquireError;
use inquire::Text;

fn handle_prompt_error<T>(res: Result<T, InquireError>) -> Option<T> {
    match res {
        Ok(val) => Some(val),
        Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
            std::process::exit(130);
        }
        Err(InquireError::NotTTY) => {
            eprintln!("error: standard input is not a terminal; specify the argument directly");
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("error: prompt failed: {err}");
            std::process::exit(1);
        }
    }
}

fn check_terminal() {
    if !std::io::stdin().is_terminal() {
        eprintln!("error: standard input is not a terminal; specify the argument directly");
        std::process::exit(1);
    }
}

/// Prompt for text input.
pub fn ask(prompt: &str) -> String {
    check_terminal();
    handle_prompt_error(Text::new(prompt).prompt()).unwrap_or_default()
}
