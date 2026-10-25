# Functions, methods, and traits

- Prefer conversion traits (`From<T>`, `TryFrom<T>`) over ad hoc `from_t`/`into_t` methods.
- If converting `T -> U` and `T` has strictly more information than `U`, put the conversion method on `T`. If the conversion is lossy (`U -> T`), it may not fit a standard trait (might fail or need extra info) — put it on `T` too. (Keeps conversions off the "lowest information" type; avoids duplication.)
- Where there's a clear receiver (`self`), prefer a method over a free function.
- `Self` vs. the explicit type name: use `Self` to mean "the current type," the explicit name to mean "that specific type." Constructors usually use the explicit name; private static-method calls usually use `Self`; methods comparing against an object of the same type usually use `Self`. When in doubt, use the explicit name.

## Functions

- Prefer short functions — as an upper bound, you should almost always be able to read one without scrolling. Split large functions into smaller ones; if that's hard, consider factoring out a data structure and turning the function into methods.
- Don't use `#[inline]`/`#[inline(never/always)]` without benchmark evidence.
- No-argument constructors should implement `Default`. The primary constructor is an inherent static `new`. A couple of other constructors with clear roles: inherent static methods prefixed `with_`. More complexity than that: use the builder pattern.
- Functions that take ownership should take the argument by value or as a `Box` — never take a reference just to clone it internally.
- Functions should prefer borrowed references unless they genuinely need ownership.
- Functions should prefer returning by value over `Box`/`Rc`/other smart pointers — lets the caller decide how to hold the result, with no forced allocation.
- Be deliberate about generics: they add flexibility and testability but also complexity. Don't add a generic parameter without justification, and don't use generics just to make conversions implicit (e.g. `fn foo(x: impl Into<bool>)` saves little over the caller calling `.into()` themselves — prefer converting at the API boundary where it makes logical sense).
- Prefer a custom enum over `bool`/`Option` arguments when it conveys meaning more clearly (readability, future-proofing).
- Validate arguments with the type system where you can; validate dynamically where you can't.
- For `Read`/`Write` bounds, take the argument by value, not by (mutable) reference.

## Methods

- Avoid getters/setters unless necessary — prefer public fields.
- A function with no clear receiver that primarily builds a data type should be a static method of that type (all constructors are static methods).
- Private static methods should usually just be free functions — modules, not types, are Rust's real privacy boundary.
- Public functions should be static methods only when they're logically part of the type's API (e.g. constructors).

## Generics

- Weigh generics/`impl Trait` against `dyn Trait` objects — generics are usually, but not always, the better choice.
- Single simple bound: use `impl Trait` syntax. Complex bounds: use a `where` clause.
- Use lifetime elision wherever the 2018-edition rules allow it.
- Function-type parameters always go in a `where` clause.
- Prefer the most flexible function-trait bound for arguments: `Fn` > `FnMut` > `FnOnce` (the reverse order applies if you're the one *implementing* a function trait: prefer implementing `FnOnce` > `FnMut` > `Fn`).
- More than ~3 generic parameters on a function is a signal it might be better as multiple functions, a data type, or that some parameters belong as associated types instead.

### Flexible function arguments

Trade-off: the generic/flexible form is more adaptable, but hurts signature readability, can bloat code, adds boilerplate, and occasionally generates less performant code. Consider:
- Is this public API? Favor flexibility. Internal-only? Favor simplicity.
- Do existing callers already convert at the call site? Flexibility can remove that duplication.
- Does the simple type commonly show up in different flavors (e.g. `str`/`OsStr`/`CStr`/`String`, or "a collection vs. an iterator over it")? Favor flexibility.
- Otherwise, favor the simple version (KISS, YAGNI).

Common patterns: `impl AsRef<T>` instead of `&T`; `impl Into<T>` instead of `T` (special case: `IntoIterator`); `Cow<'_, T>` to accept either owned or borrowed (special case `Cow<'static, T>`, useful when a function usually takes a `const`/`static` value but occasionally a custom one, e.g. in tests — avoids an unnecessary allocation in the common case).

## Traits

- Use traits for most of a type's behavior; reserve inherent methods for operations on the concrete data itself.
- Prefer small traits modeling one aspect of behavior — think "aspect," not "bag of functionality."
- Use the extension-trait pattern (`Foo` + `FooExt`) to separate a core, object-safe trait from extended behavior, or to add behavior to a trait from a downstream crate.
- Group all required methods together, then all provided methods together.

## Associated types

Generic type parameters are chosen by the *caller* of a trait; associated types are chosen by the *implementer*. Give an associated type a default when you can — it keeps adding the associated type backward-compatible.
