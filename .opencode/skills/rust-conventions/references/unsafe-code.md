# Unsafe code

- Avoid `unsafe` where possible.
- If `unsafe` is for performance, back it with benchmark evidence of a real improvement.
- Rust's `unsafe` has *stricter* semantics than C around undefined behavior — be careful.
- Prefer `MaybeUninit` to any raw use of uninitialized memory; almost any other use of uninitialized memory is UB.
- Where possible, wrap `unsafe` code in a safe function that re-establishes all invariants before returning.
- Be especially careful with `unsafe impl` — its safety effects can be felt far from the declaration.
- Avoid `transmute` where possible. If you must use it, specify both type parameters explicitly via turbofish, e.g. `mem::transmute::<&'b R<'static>, &'b R<'c>>(r)`.
- Unsafe code must validate its inputs with extra care.
- Size unsafe blocks for whatever's convenient — the historical advice to make them "as small as possible" undersells that unsafety is often inherently non-local; use the scope that best fits the code.
- Prefer `bindgen`-generated FFI over hand-written FFI.
- Prefer `std::mem`/`std::ptr` functions over lower-level manual pointer manipulation.
- See also the [Rustonomicon](https://doc.rust-lang.org/nomicon/).

## Documentation

- Document *every* use of `unsafe`, including trivial ones and `unsafe impl` — even a one-liner needs a comment on why it's safe.
- Explain why the block is safe and how Rust's invariants are maintained.
- For `unsafe fn`, document the invariants callers must uphold, input restrictions, and constraints on how the output may be used.

## Unsafe boundaries

Rust's core invariants, which must hold *at all times* (in both safe and unsafe code — they don't have to hold across the FFI boundary in C or other languages, but must be re-established before returning to Rust):

- Multiple references to a value ⇒ the value must be immutable.
- A mutable value ⇒ any reference to it must be unique (corollary: no concurrent access from multiple threads if any access is a write).
- Pointers (other than raw pointers) are never null.
- A `&T` (or similar pointer-to-`T`) must point to initialized memory of at least `size_of::<T>()`.

You don't need to document these baseline invariants themselves, but it can help to note *why* or *how* they're maintained in a specific tricky case.

Beyond that baseline, most `unsafe` code also has program-specific invariants — sometimes none at all (e.g. a simple FFI call where any valid Rust state is fine), sometimes quite involved. Breaking one of these can either break a core Rust invariant (UB, memory unsafety) or cause a logic error *anywhere in the program*, not just in the unsafe block. **Always document these.**

- Some invariants are local to a single `unsafe` block — e.g. "this pointer points to at least `n * sizeof(T)` bytes of initialized memory." Wrap the unsafe code in a small safe function that establishes exactly that invariant and nothing more:

  ```rust
  /// Call the `foo` function defined in `some_library/foo.h`.
  ///
  /// `Bar` must be `repr(C)`.
  fn call_foo(input: &[Bar]) {
      let ptr = input.as_ptr();
      let len = input.len();
      // Calling `foo` is safe since `ptr` and `len` are a valid representation of `input`.
      unsafe {
          let result = foo(ptr, len);
          // `foo` will never return an error if the input is valid.
          debug_assert_eq!(result, 0);
      }
  }
  ```
  Note the safety comment is present even though the function is tiny — always explain why, even when it looks obvious.

- More complex invariants span *multiple* unsafe blocks, where a bug in one can surface as an error in a different one. There, the boundary is the **module**: keep it as small as practical, and use Rust's privacy so that public functions are always safe to call with any input/state, while private functions may be `unsafe` (or safe-but-relying-on-internal-invariants, in which case they should be `unsafe` if bad input/state could cause an error).

  The standard library's `Vec` is a good example: the [`vec`](https://github.com/rust-lang/rust/blob/master/src/liballoc/vec.rs) module's small amount of unsafe code relies on simple, easy-to-maintain invariants for its callers, while the more complex, heavily-unsafe internals live in [`raw_vec`](https://github.com/rust-lang/rust/blob/master/src/liballoc/raw_vec.rs) and stay well encapsulated underneath. Document invariants at the module level *and* on unsafe functions *and* on every unsafe block.
