//! HTML entity escaping, tag stripping, and rich-text excerpt generation.

mod escape;
mod excerpt;
mod strip;

pub use escape::escape_html;
pub use excerpt::excerpt;
pub use strip::strip_html_tags;
