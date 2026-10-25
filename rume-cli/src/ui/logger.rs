//! Terminal UI helpers for the Rume CLI.
//!
//! Provides colored, leveled logging and other terminal primitives similar to
//! AdonisJS Ace's terminal UI, built on top of the `console` crate.

use console::{Style, Term};
use std::io::Write;

/// Log level for filtering output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Success,
    Warning,
    Error,
}

/// A simple colored logger for CLI output.
pub struct Logger {
    term: Term,
    colors: bool,
    min_level: Level,
}

impl Logger {
    pub fn new(colors: bool) -> Self {
        Self {
            term: Term::stdout(),
            colors,
            min_level: Level::Info,
        }
    }

    pub fn with_min_level(mut self, level: Level) -> Self {
        self.min_level = level;
        self
    }

    pub fn debug(&self, msg: &str) {
        if self.min_level <= Level::Debug {
            self.log(Level::Debug, msg, None);
        }
    }

    pub fn info(&self, msg: &str) {
        if self.min_level <= Level::Info {
            self.log(Level::Info, msg, None);
        }
    }

    pub fn success(&self, msg: &str) {
        if self.min_level <= Level::Success {
            self.log(Level::Success, msg, None);
        }
    }

    pub fn warning(&self, msg: &str) {
        if self.min_level <= Level::Warning {
            self.log(Level::Warning, msg, None);
        }
    }

    pub fn error(&self, msg: &str) {
        self.log(Level::Error, msg, None);
    }

    pub fn info_with(&self, msg: &str, suffix: Option<&str>, prefix: Option<&str>) {
        if self.min_level <= Level::Info {
            self.log(Level::Info, msg, suffix);
            let _ = prefix;
        }
    }

    fn log(&self, level: Level, msg: &str, suffix: Option<&str>) {
        let (icon, style) = if self.colors {
            match level {
                Level::Debug => ("🔍", Style::new().dim()),
                Level::Info => ("ℹ", Style::new().blue()),
                Level::Success => ("✔", Style::new().green()),
                Level::Warning => ("⚠", Style::new().yellow()),
                Level::Error => ("✖", Style::new().red()),
            }
        } else {
            match level {
                Level::Debug => ("", Style::new()),
                Level::Info => ("", Style::new()),
                Level::Success => ("", Style::new()),
                Level::Warning => ("", Style::new()),
                Level::Error => ("", Style::new()),
            }
        };

        let styled_msg = if self.colors {
            style.apply_to(msg).to_string()
        } else {
            msg.to_string()
        };

        let line = match suffix {
            Some(s) => format!("{} {}  {}", icon, styled_msg, s),
            None => format!("{} {}", icon, styled_msg),
        };

        let mut term = self.term.clone();
        let _ = writeln!(term, "{}", line);
    }
}

/// Check if the terminal supports colors.
pub fn colors_enabled() -> bool {
    console::Term::stdout().features().colors_supported()
}

/// Create a logger with auto-detected color support.
pub fn logger() -> Logger {
    Logger::new(colors_enabled())
}
