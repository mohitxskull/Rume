# Task 12 — Split `crates/foundation/src/util/html.rs` (324) into `util/html/{mod,escape,strip,excerpt}.rs`

## Goal

Split `crates/foundation/src/util/html.rs` into `crates/foundation/src/util/html/` modules —
behavior-preserving; `cargo build` + `cargo test -p rume-foundation` green.

Judgment (conductor Q "split util/html.rs if it mixes text/HTML concerns"): the file mixes
concerns — `escape_html` (html.rs:24-45) is an HTML-escaping function, `strip_html_tags`
(html.rs:65-239) is an HTML/XML-stripping function, while `excerpt` (html.rs:252-263) is a
plain-text excerpt built from `strip_html_tags` + `condense_whitespace` + `truncate` (a text
concern). It is split.

## Files touched

- `crates/foundation/src/util/html.rs` — deleted; replaced by:
- `crates/foundation/src/util/html/mod.rs` — module doc (html.rs:1) + re-exports
  `pub use escape::escape_html; pub use strip::strip_html_tags; pub use excerpt::excerpt;`
  (so `rume_foundation::util::html::{escape_html, strip_html_tags, excerpt}` resolve unchanged;
  the moved doc examples now read `rume_foundation::util::html::…`).
- `crates/foundation/src/util/html/escape.rs` — `escape_html` (html.rs:24-45) + its tests
  (html.rs:269-283: `escape_html_clean_input_borrows`, `escape_html_escapes_entities`).
  Imports: `use std::borrow::Cow;`.
- `crates/foundation/src/util/html/strip.rs` — `strip_html_tags` (html.rs:65-239) + its tests
  (html.rs:286-301 `strip_html_tags_removes_tags`; html.rs:309-323
  `strip_html_tags_handles_quoted_gt_and_unclosed_lt` including the `Price < 100 USD`,
  `<_item>x</_item>`, `<é>x</é>`, `<!-- don't show -->Visible` cases and the repeated-
  unterminated-comment linear-time loop at html.rs:320-322). Imports: `use std::borrow::Cow;`.
- `crates/foundation/src/util/html/excerpt.rs` — `excerpt` (html.rs:252-263) + its tests
  (html.rs:303-307 `excerpt_strips_and_truncates`; the `<!-- don't show -->Visible` excerpt case
  at html.rs:317-318). Imports: `use std::borrow::Cow;` + `use crate::util::truncate::truncate;`
  + `use crate::util::whitespace::condense_whitespace;` (paths fixed: the original file used
  `super::truncate`/`super::whitespace`, which resolve to `util::truncate`/`util::whitespace`;
  from `util/html/*` the direct `crate::util::` paths are used).

## Deliverable

- `rume_foundation::util::html::{escape_html, strip_html_tags, excerpt}` — same public surface as
  before (`util/html/mod.rs` re-exports).
- Tests live in the per-concern files (`util/html/escape.rs`, `util/html/strip.rs`,
  `util/html/excerpt.rs`).

## Constraints honoured

- Public API: `rume_foundation::util::html::escape_html` / `strip_html_tags` / `excerpt` resolve
  (via `util/html/mod.rs` re-exports).
- No behavior change (function bodies move verbatim; only import paths adjusted).
- Global: `cargo build` + `cargo test -p rume-foundation` green.

## Named tests (bodies omitted — moved from `crates/foundation/src/helpers/html.rs` into
`util/html/{escape,strip,excerpt}.rs`)

From `util/html/escape.rs`:
- `escape_html_clean_input_borrows` — asserts clean input returns `Cow::Borrowed` and equals input.
- `escape_html_escapes_entities` — asserts `a & b < c > d "e" 'f'` escapes to
  `a &amp; b &lt; c &gt; d &quot;e&quot; &#x27;f&#x27;`.

From `util/html/strip.rs`:
- `strip_html_tags_removes_tags` — asserts tag-stripping on `<div><p>Hello <br/>World</p></div>` →
  `Hello World`, `<p>Hello <strong>World</strong>!</p>` → `Hello World!`,
  `<span>Hello</span><span>World</span>` → `Hello World`, and `Fish &amp; Chips` → `Fish & Chips`.
- `strip_html_tags_handles_quoted_gt_and_unclosed_lt` — asserts `<a title="a > b">Click</a>` → `Click`,
  `<a title='x > y'>Link</a>` → `Link`, `Price < 100 USD` preserved, `<p>Price < 100 USD</p>` →
  `Price < 100 USD`, `<_item>x</_item>` → `x`, `<é>x</é>` → `x`,
  `<!-- don't show -->Visible` → `Visible`, and the repeated unterminated `<!--x` loop returns
  input unchanged.

From `util/html/excerpt.rs`:
- `excerpt_strips_and_truncates` — asserts
  `excerpt("<div><h1>Hello World</h1><p>This is a paragraph.</p></div>", 15)` == `"Hello World..."`.

## Verification

```
cargo build
cargo test -p rume-foundation
```

## Commit

Only when the user asks:

```
refactor(foundation): split util/html.rs into util/html/ escape, strip, and excerpt modules
```

## Done-when

- [ ] `crates/foundation/src/util/html.rs` deleted; `crates/foundation/src/util/html/` contains
  `mod.rs`, `escape.rs`, `strip.rs`, `excerpt.rs`.
- [ ] `rume_foundation::util::html::{escape_html, strip_html_tags, excerpt}` resolve.
- [ ] `cargo build` + `cargo test -p rume-foundation` green.
