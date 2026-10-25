//! Sticker / boxed content for highlighting important info.

use console::Style;

/// Render boxed content (sticker).
pub struct Sticker {
    lines: Vec<String>,
    color: Option<Style>,
}

impl Sticker {
    pub fn new() -> Self {
        Self {
            lines: Vec::new(),
            color: None,
        }
    }

    pub fn add(&mut self, line: &str) -> &mut Self {
        self.lines.push(line.to_string());
        self
    }

    pub fn render(&self) {
        let max_len = self.lines.iter().map(|l| l.len()).max().unwrap_or(0);
        let border = "─".repeat(max_len + 2);
        println!("┌{}┐", border);
        for line in &self.lines {
            let padding = " ".repeat(max_len - line.len());
            println!("│ {}{} │", line, padding);
        }
        println!("└{}┘", border);
    }
}

impl Default for Sticker {
    fn default() -> Self {
        Self::new()
    }
}

/// Render instructions list with arrow prefix.
pub struct Instructions {
    lines: Vec<String>,
}

impl Instructions {
    pub fn new() -> Self {
        Self { lines: Vec::new() }
    }

    pub fn add(&mut self, line: &str) -> &mut Self {
        self.lines.push(line.to_string());
        self
    }

    pub fn render(&self) {
        for line in &self.lines {
            println!("  > {}", line);
        }
    }
}

impl Default for Instructions {
    fn default() -> Self {
        Self::new()
    }
}
