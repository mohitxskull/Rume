# Formatting

- Use Rustfmt (`cargo fmt --all`) with **no customization** (no `rustfmt.toml`), except temporary workarounds. Install with `rustup component add rustfmt`.
- Rustfmt won't format everything — notably some `macro_rules!` bodies and macro invocations. In those spots, follow the [Rust style guide](https://github.com/rust-lang/rfcs/blob/master/style-guide/README.md) by hand.
  - If unformatted macro content is code-like, follow the formatting style guide as closely as possible.
  - If it's tabular data, align columns with spaces (not tabs) at multiple-of-four column positions.
- Always use Unix line endings (`\n`).

**Rationale:** consistency with the Rust ecosystem lowers the barrier to entry; self-consistency makes reading easier.

## Imports

- Avoid `extern crate`, except for macro crates used in many files (e.g. logging macros). Group any remaining `extern crate`s together.
- Group all imports together; group re-exports (`pub use ...;`) together separately.
- Use a single nested import per crate rather than multiple `use` lines for the same crate (Rustfmt's `merge_imports` option does this mechanically).
- Prefer absolute paths (`crate::...`) over relative paths (`super::...`, `self::...`).
  - Exception: re-exporting from a submodule, e.g. `pub use self::foo::bar;`.

Example:

```rust
use crate::{
    pd::client::{Cluster, RetryClient},
    util::GLOBAL_TIMER_HANDLE,
    Result,
};
use futures::{
    compat::Compat01As03,
    prelude::*,
    ready,
    task::{Context, Poll},
};
use std::{
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

pub use self::Request;

#[cfg(test)]
mod test {
    use super::*;
    use tokio_timer::timer::Handle;
}
```

**Rationale:** ecosystem/modern conventions; makes it easier to trace where names come from without IDE support, see module dependencies at a glance, and move imports between modules with less duplication.
