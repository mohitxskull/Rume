# Macros

- Macro definitions are much easier to write than to read — prefer functions or traits where they'd work instead. Only reach for a macro when there's a substantial benefit, usually cutting significant boilerplate.
- Prefer `macro_rules!` (declarative) macros over procedural macros.
- Prefer an existing library (e.g. Serde) over writing a new macro.
- If you do write a procedural macro, use [Syn](https://github.com/dtolnay/syn) for parsing and [Quote](https://github.com/dtolnay/quote) for quasi-quoting.
- Don't use the "proc macro hack" to use procedural macros in item position.
- Custom derive macros shouldn't do complex code generation — use an attribute-position procedural macro for that instead. Custom derives are best for auto-deriving traits, and can also generate very simple methods (e.g. `new` constructors via `derive-new`/`derive-more`, or getters/setters via `Getset` — though public fields are still generally preferred over generated getters/setters).

## Formatting

Rustfmt sometimes won't format macro definitions or uses. In those spots, follow Rustfmt's conventions as closely as possible by hand — see the [Rust style guide](https://github.com/rust-lang/rfcs/tree/master/style-guide).
