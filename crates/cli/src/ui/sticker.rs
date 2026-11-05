//! Sticker / boxed content for highlighting important info.

use unicode_width::UnicodeWidthStr;

/// Render boxed content (sticker).
pub struct Sticker {
    lines: Vec<String>,
}

impl Sticker {
    pub fn new() -> Self {
        Self {
            lines: Vec::new(),
        }
    }

    pub fn add(&mut self, line: &str) -> &mut Self {
        self.lines.push(line.to_string());
        self
    }

    pub fn render_to_string(&self) -> String {
        let max_width = self
            .lines
            .iter()
            .map(|l| UnicodeWidthStr::width(l.as_str()))
            .max()
            .unwrap_or(0);
        let border = "─".repeat(max_width + 2);
        let mut out = String::new();
        out.push_str(&format!("┌{}┐\n", border));
        for line in &self.lines {
            let line_width = UnicodeWidthStr::width(line.as_str());
            let padding = " ".repeat(max_width.saturating_sub(line_width));
            out.push_str(&format!("│ {}{} │\n", line, padding));
        }
        out.push_str(&format!("└{}┘\n", border));
        out
    }

    pub fn render(&self) {
        print!("{}", self.render_to_string());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sticker_aligns_with_unicode_characters() {
        let mut sticker = Sticker::new();
        sticker.add("Hello World");
        sticker.add("🚀 Launch");
        sticker.add("café ☕");
        let rendered = sticker.render_to_string();
        for line in rendered.lines() {
            // Every box line should have the exact same terminal display width
            assert_eq!(
                UnicodeWidthStr::width(line),
                UnicodeWidthStr::width("┌─────────────┐"),
                "Line display width should match top border: {line}"
            );
        }
    }
}
