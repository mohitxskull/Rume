# Expressions and statements

- Clippy has extensive statement-level advice ([lint list](https://rust-lang.github.io/rust-clippy/master/)) — useful, but some lints are overly pedantic; use judgment.
- Don't use `println!`/similar for logging or debugging in committed code — use a logging library.
- Use variable shadowing judiciously: good when the value is logically the same thing referenced differently (`let x = ...; let x = Rc::new(x);`), bad when you're reusing a name for a logically different value — that gets confusing fast.
- Prefer functions over closures when the closure doesn't capture anything from its environment — makes it obvious there's no implicit scoping going on.
- Use [`std::mem::replace`](https://doc.rust-lang.org/nightly/std/mem/fn.replace.html) to move data out of a `&mut` (see also the [mem::replace idiom](https://github.com/rust-unofficial/patterns/blob/master/idioms/mem-replace.md)).
- Prefer expression-oriented code to statement-oriented code (see the [Rust style guide](https://github.com/rust-lang/rfcs/blob/master/style-guide/advice.md#expressions)).
- Avoid complex code directly inside `if`/`match`/similar conditions — pull it into a named variable; it's almost always clearer.
- Prefer `into` over `from` when you have a choice.
- Don't use `let _ =` to silence an unused-`Result` warning — use `?` or `expect` (or `unwrap` as a last resort). Note `let _foo = ...` and `let _ = ...` differ: the latter drops immediately (no scoped variable), the former drops at end of scope — matters if the value has a destructor.
- Avoid `_` as an intermediate type in macros/casts (e.g. `&*(foo as *const _ as *const B)`) — spell out the full type; it's safer against later changes and easier to read. (Using `_` as a type *parameter*, e.g. `Vec<_>` to help `collect()` infer, is fine.)
- Use the [`Entry` API](https://doc.rust-lang.org/nightly/std/collections/hash_map/enum.Entry.html) for maps.
- Prefer an explicit type annotation on the variable over turbofish where you have the choice; prefer turbofish over type ascription. E.g. `let x: Vec<_> = iter.collect();`, not `let x = iter.collect::<Vec<_>>();`.
- Use parentheses instead of relying on operator precedence — easier to read and avoids subtle bugs once this becomes habitual.
- Use field-init shorthand: `Foo { x, y }`, not `Foo { x: x, y: y }`.
- Call methods with an explicit receiver: `x.clone()`, not `Arc::clone(x)` — less clutter, more uniform. (The old advice to prefer `Arc::clone(x)` is outdated.)

## Mutability

- Prefer shadowing to mutability where it works — restricts unwanted mutation and lets the mutability lints do more for you.
- Minimize the scope of mutability with shadowing and/or explicit `{ ... }` blocks (see the [temporary mutability idiom](https://github.com/rust-unofficial/patterns/blob/master/idioms/temporary-mutability.md)).

## Iteration

- Never iterate over a collection without an iterator — manual indexing is slower and more error-prone.
- Prefer iterator combinators (`map`, `filter`, ...) over imperative `for` loops — but switch to imperative iteration if a large closure or side effects make that clearer.
- Don't `collect()` just to force iteration when you don't need the result — use `for_each` or a plain loop (clearer intent, no unneeded allocation).

## `match` and `if`

- Prefer method calls (especially `Option`/`Result` combinators) over `match` where they exist.
- Avoid matching on every field of a struct — adding a field later becomes a breaking change to that match.
- List patterns explicitly rather than falling back to a wildcard `_` — fail fast if an enum grows a variant later.
- Let the compiler's match ergonomics handle borrowing; avoid explicit `ref` patterns (see the [match ergonomics RFC](https://github.com/rust-lang/rfcs/blob/master/text/2005-match-ergonomics.md)).
- Condense multiple wildcards in one pattern into a single `..`.
- Prefer `if let` over a `match` with one arm. Prefer `match` over `if let ... else`. (Use whichever is clearest and most concise for the situation.)
- In `if`/`else`, put the positive condition first: `if foo { a() } else { b() }`, not `if !foo { b() } else { a() }`.
- Prefer a guarded early return over an `if`/`else` where one branch is much larger than the other:
  ```rust
  fn foo(x: i32) -> u8 {
      if x != 0 {
          return 42;
      }
      // ...
      0
  }
  ```
  rather than
  ```rust
  fn foo(x: i32) -> u8 {
      if x == 0 {
          // ...
          0
      } else {
          42
      }
  }
  ```

## Panicking

- Don't "hide" panicking code — make it obvious via naming and documentation. See `references/errors-and-panics.md` for the fuller `unwrap`/panic guidance.
- Prefer `debug_assert!` when the condition should always be true; use `assert!` when it might reasonably be false due to a bug/error (even if unlikely). If the condition could fail with no bug involved, check it without panicking.
- Integer arithmetic panics on overflow in debug builds and wraps in release builds — only use `+`/`-`/`*` shorthand when overflow truly can't happen. Otherwise use `wrapping_add`, `saturating_add`, `overflowing_add`, `checked_add`, etc.
