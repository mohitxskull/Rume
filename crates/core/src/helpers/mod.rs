//! Pure, hermetic application helpers.
//!
//! Inspired by AdonisJS `@adonisjs/helpers` but built with Rust strengths:
//! zero-allocation fast paths via `Cow<'a, str>`, UTF-8 character boundary safety,
//! strongly typed error enums, and constant-time comparison hardening with `black_box`.
//!
//! Everything is implemented in `std` and workspace `thiserror` — no new dependencies,
//! no `unsafe`, and `#![forbid(unsafe_code)]` compliant.

pub mod bytes;
pub mod case;
pub mod duration;
pub mod html;
pub mod mask;
pub mod ordinal;
pub mod safe_equal;
pub mod sentence;
pub mod slug;
pub mod truncate;
pub mod whitespace;

// Re-export the public helpers surface at the module root for convenience.
pub use bytes::{ParseBytesError, format_bytes, parse_bytes};
pub use case::{
    camel_case, dot_case, kebab_case, pascal_case, sentence_case, snake_case, title_case,
};
pub use duration::{ParseDurationError, format_duration, parse_duration};
pub use html::{escape_html, excerpt, strip_html_tags};
pub use mask::{mask_email, mask_secret};
pub use ordinal::{ordinal, ordinal_suffix};
pub use safe_equal::{safe_equal, safe_equal_hashed};
pub use sentence::{to_sentence, to_sentence_with};
pub use slug::slug;
pub use truncate::{TruncateOptions, truncate, truncate_with};
pub use whitespace::condense_whitespace;
