# Implementing traits

## Organization

Order of `impl` blocks:
1. inherent `impl`s
2. `impl`s for important traits
3. `impl`s for marker traits (`Sync`, `Send`)
4. `impl`s for utility traits (`Debug`, `Into`)

- Group `impl`s for the same trait together.
- Put `impl`s directly after their concrete type; put blanket `impl`s directly after the trait they're for.
- Prefer keeping all `impl`s in one module — split across modules only if it genuinely improves readability for very large `impl`s.
- Multiple `impl` blocks with the same signature in one module are fine if the grouping aids readability (Rustdoc preserves the grouping, and doc-comments on an `impl` act as an intro to that group of methods).
- Published/widely-used crates: implement as many relevant std traits as reasonable, for flexibility/interop. Unpublished crates: implement traits as needed (YAGNI).
- Prefer `derive` over a manual `impl` when possible.
- Prefer implementing an existing trait over adding an inherent method that duplicates it (e.g. implement `Default` rather than a no-arg `new`).
- Be very careful with `unsafe impl` — it can introduce unsafety with effects far from the `impl` itself.

**Rationale:** code near data; important things first; fixed order aids findability; less boilerplate; more usable with generic code.

## Follow documented trait semantics

- Cross-trait invariants matter — e.g. if `Hash` is implemented, values equal under `Eq` must hash equally.
- Traits carry semantic intent beyond their method signatures: `Debug` and `Display` share method shapes but different purposes — never use `{:?}` (`Debug`) in user-facing output like error messages (fine in logs).
- Where multiple similar traits exist, there's often a preferred one — e.g. prefer implementing `From` over `Into` (there's a blanket `impl` in the other direction).
- Prefer the fallible variant when relevant (`TryFrom` vs `From`).
- `AsRef`/`Borrow` are similar but not the same: `Borrow` requires consistent `Eq`/`Ord`/`Hash`; `AsRef` is more flexible. If you implement `AsMut`, implement `AsRef` too — both should be cheap. If you implement `Deref`, implement `AsRef` or `Borrow` too. If you implement `Borrow`, implement `ToOwned` for the inverse.
- Marker traits like `Sync`/`Unpin` carry strong semantics — respect them.

## Common traits

- Implement `Debug` wherever possible; keep the output meaningful but not so long it overwhelms logs.
- Collections should implement `Iterator` (typically via a dedicated iterator type) and `IntoIterator`, and may also offer an inherent `iter` for a borrowed iterator (see `slice::iter`).
- Only implement operator traits (`Add`, `Mul`, etc.) where the operation is obvious and unambiguous — e.g. don't implement `Mul` for a vector type (cross vs. dot product is ambiguous), but `Add` is fine. When in doubt, restrict operator impls to numeric/logical types.
- Only implement `Index` for collection types. Most collections should implement `Index`, `IntoIterator`, `FromIterator`; most mutable collections should implement `Extend`.
- Only implement `Deref` for smart pointers, never for general coercion. Smart pointers should usually implement `Deref` and `Borrow` together.
- Implement `Drop` for types that need cleanup. `Drop` impls must never panic (watch for implicit panics like bounds-check indexing) and should not block. If shutdown can fail/panic/block, expose an explicit method (e.g. `close`) returning a `Result`, so the user can call it and check errors; if `Drop` runs without that method having been called, it's fine for `Drop` to panic (documented) so tests catch the omission.
