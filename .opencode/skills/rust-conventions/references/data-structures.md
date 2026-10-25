# Data structures

- For generic data types, use the most flexible bounds possible: put strict bounds on `impl`s, not the concrete type (split `impl`s if needed). Generic parameters on the concrete type should only be bounded if the bound is intrinsic to the type itself (lifetime bounds, `?Sized`, a bound required to name an associated type, or one required by a `Drop` impl). Never put derivable bounds on generic parameters of a concrete type. (Rationale: flexibility, less duplication, better backward compatibility.)
- Avoid default generic parameters unless the default covers the vast majority of uses (e.g. only overridden for testing). An explicit type usually improves comprehension more than the convenience is worth.
- Use types to enforce invariants — structs, tuples, and enums to define valid states; associated types to enforce relationships between types. Make invalid states unrepresentable where possible.
- Use `type` aliases for convenience versions of common generics (e.g. `type Result<T> = Result<T, MyErr>;`).

## Structs

- Prefer several small structs composed into a larger one over one big struct (more borrow-checker-friendly, more flexible, often better design).
- Think carefully about field visibility: public fields are API and a public field can't change without breaking compatibility; prefer public fields over getters/setters; if an invariant spans multiple fields, all of those fields must be private; if *every* field is public, adding a new field is itself a breaking change.
- Empty structs: `struct Foo;` — not `struct Foo {}` or `struct Foo();` (reserve those for when a macro requires them).
- `repr(C)` structs: field order is significant (usually for FFI). If order isn't constrained, sort fields by descending size for the most efficient layout.
- Use tuple structs only when there's exactly one field and its meaning is obvious (a plain wrapper).
- Use the newtype pattern for structs that share representation but need different behavior.

## Enums

- Many variants are fine.
- An enum's size is the size of its largest variant — very unevenly-sized variants can be inefficient.
- Variants shouldn't have more than ~3 fields; for more complex variants, give the variant a single struct field (named the same as the variant) and move the fields there.
- Avoid named fields directly in variants — if a struct variant would help, a genuine named struct is usually even better.
- Avoid empty variants: prefer `Foo` over `Foo()`/`Foo {}`.
- If an enum has a natural "null" value, give it a `None` variant rather than wrapping the whole enum in `Option` (same idea for `Result`/`Err`). `Option<Foo>` means "may be null sometimes"; a `None` variant on `Foo` means something different, and simplifies matching.

## Tuples

- Use a tuple for several logically separate pieces of data; prefer a struct for anything that represents one logical unit. Tuples are fine for "temporary" data meant to be destructured immediately (e.g. returning two values to be destructured at the call site).
- Only use a tuple when the meaning is clearly "multiple distinct types" — not as a shortcut where a named type would be clearer. Prefer `Point { x: i32, y: i32 }` to `(i32, i32)`.
- A tuple with multiple elements of the *same* type is almost always a bad idea.

## Unions

Only use `union` for FFI mirroring a C union/enum. Use an `enum` in ordinary Rust code.

## Builder pattern

Use the [builder pattern](https://doc.rust-lang.org/1.0.0/style/ownership/builders.html) to avoid a sprawl of constructor functions or a constructor with many `Option` parameters — but don't reach for it in place of one or two simple constructors.
