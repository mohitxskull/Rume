//! Animated task runner for multi-step operations.

use console::Style;
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

/// A simple task runner that shows progress for each step.
pub struct TaskRunner {
    verbose: bool,
}

impl TaskRunner {
    pub fn new(verbose: bool) -> Self {
        Self { verbose }
    }

    /// Run a single named task with a spinner.
    pub fn run_task<F>(&self, name: &str, f: F) -> Result<(), String>
    where
        F: FnOnce(&TaskHandle) -> Result<String, String>,
    {
        let spinner = ProgressBar::new_spinner();
        spinner.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} {msg}")
                .unwrap(),
        );
        spinner.set_message(name.to_string());
        spinner.enable_steady_tick(Duration::from_millis(80));

        let handle = TaskHandle {
            verbose: self.verbose,
        };

        match f(&handle) {
            Ok(msg) => {
                spinner.finish_with_message(format!("✔ {} — {}", name, msg));
                Ok(())
            }
            Err(err) => {
                spinner.finish_with_message(format!("✖ {} — {}", name, err));
                Err(err)
            }
        }
    }
}

pub struct TaskHandle {
    verbose: bool,
}

impl TaskHandle {
    pub fn update(&self, msg: &str) {
        if self.verbose {
            println!("    {}", msg);
        }
    }

    pub fn is_verbose(&self) -> bool {
        self.verbose
    }
}

/// Convenience: style text green.
pub fn green(text: &str) -> String {
    Style::new().green().apply_to(text).to_string()
}
