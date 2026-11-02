//! Pure, hermetic string helpers.
//!
//! Inspired by AdonisJS `@adonisjs/helpers` (`safeEqual`, `slug`, case
//! conversions). Everything is implemented in `std` — no new dependencies,
//! no `unsafe`. Input is treated as ASCII; non-ASCII characters are passed
//! through unchanged except where lowercasing is explicitly applied.

pub mod case;
pub mod safe_equal;
pub mod slug;

// Re-export the public helpers surface at the module root for convenience.
pub use case::{camel_case, kebab_case, pascal_case, snake_case};
pub use safe_equal::safe_equal;
pub use slug::slug;
