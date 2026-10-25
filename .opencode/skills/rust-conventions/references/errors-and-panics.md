# Error handling

- Errors are part of your API — design them with the same care as the rest of it.
- Follow the error-handling approach already used in the surrounding code; don't introduce a new library/technique for one function unless you're converting the whole crate.
- Don't invent your own `Result` type from scratch (a type alias like `type Result<T> = std::result::Result<T, MyErr>;` is fine).
- Don't use `Option` to signal an error — `Option` means "may or may not exist," where both states are valid, not "failed."
- Use `?`, never `try!` (the old macro).
- Prefer `?` or `Result`'s combinator methods over `match` — more concise and readable.
- Error types should compose: it should be possible to nest and abstract over them.
- Error types should implement `Debug`, `Display`, and `std::error::Error` (debuggability, interop).
- Error types should implement `Sync + Send` — errors commonly cross threads. If using a trait object, bound it `Error + Send + Sync + 'static`.
- Prefer preserving context in an error (e.g. chaining causes) over discarding it.
- Prefer structured data in errors over pre-stringified messages — structure lets you build better messages later with more debugging info.

## Panicking

- Prefer returning `Result` over panicking — it leaves the choice to panic with the caller. Prefer `?` over `unwrap`. Errors can bubble all the way up to the client if there's no local recovery. Exception: if the process itself can recover from a crash, panicking is acceptable.
- Prefer `expect("short message")` over bare `unwrap()` — exception: unlocking a mutex or similar, where failure is unlikely, unrecoverable anyway, and the reason is well understood by any reader.
- `unwrap`/`expect` should only appear where failure is provably impossible (e.g. right after `is_ok()` — though `if let` is usually cleaner there) or where recovery is genuinely impossible (e.g. mutex poisoning).
- `unwrap`/`expect` are fine to use freely in tests.
- If a function is meant to panic under non-exceptional use, say so in its name (typically via `unwrap`/`assert` in the name) and document the panicking behavior.
- Remember indexing (`v[i]`) can panic.

**Rationale:** `Result` is for recoverable errors, panics are for unrecoverable ones — but TiKV specifically can recover from a process crash, which is why panicking is a more acceptable fallback here than in some other contexts. Making panics as debuggable as possible matters when they do happen.
