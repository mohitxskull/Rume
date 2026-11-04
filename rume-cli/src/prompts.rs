//! Interactive prompt helpers using `inquire`.

use std::io::IsTerminal;
use inquire::InquireError;

pub use inquire::Confirm;
pub use inquire::MultiSelect;
pub use inquire::Password;
pub use inquire::Select;
pub use inquire::Text;

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

/// Prompt for text input with a default.
pub fn ask_default(prompt: &str, default: &str) -> String {
    check_terminal();
    handle_prompt_error(Text::new(prompt).with_default(default).prompt())
        .unwrap_or_else(|| default.to_string())
}

/// Prompt for a password (masked input).
pub fn secure(prompt: &str) -> String {
    check_terminal();
    handle_prompt_error(Password::new(prompt).prompt()).unwrap_or_default()
}

/// Prompt for yes/no confirmation.
pub fn confirm(prompt: &str) -> bool {
    check_terminal();
    handle_prompt_error(Confirm::new(prompt).prompt()).unwrap_or(false)
}

/// Single-select from a list of options.
pub fn select(prompt: &str, options: &[&str]) -> Option<String> {
    check_terminal();
    handle_prompt_error(Select::new(prompt, options.to_vec()).prompt()).map(|s| s.to_string())
}

/// Multi-select from a list of options.
pub fn multi_select(prompt: &str, options: &[&str]) -> Vec<String> {
    check_terminal();
    handle_prompt_error(MultiSelect::new(prompt, options.to_vec()).prompt())
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.to_string())
        .collect()
}
