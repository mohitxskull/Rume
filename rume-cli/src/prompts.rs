//! Interactive prompt helpers using `inquire`.

pub use inquire::Confirm;
pub use inquire::MultiSelect;
pub use inquire::Password;
pub use inquire::Select;
pub use inquire::Text;

/// Prompt for text input.
pub fn ask(prompt: &str) -> String {
    Text::new(prompt).prompt().unwrap_or_default()
}

/// Prompt for text input with a default.
pub fn ask_default(prompt: &str, default: &str) -> String {
    Text::new(prompt)
        .with_default(default)
        .prompt()
        .unwrap_or_else(|_| default.to_string())
}

/// Prompt for a password (masked input).
pub fn secure(prompt: &str) -> String {
    Password::new(prompt).prompt().unwrap_or_default()
}

/// Prompt for yes/no confirmation.
pub fn confirm(prompt: &str) -> bool {
    Confirm::new(prompt).prompt().unwrap_or(false)
}

/// Single-select from a list of options.
pub fn select(prompt: &str, options: &[&str]) -> Option<String> {
    Select::new(prompt, options.to_vec())
        .prompt()
        .ok()
        .map(|s| s.to_string())
}

/// Multi-select from a list of options.
pub fn multi_select(prompt: &str, options: &[&str]) -> Vec<String> {
    MultiSelect::new(prompt, options.to_vec())
        .prompt()
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.to_string())
        .collect()
}
