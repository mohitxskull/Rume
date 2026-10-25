# Performance

"Premature optimization is the root of all evil" — optimize only when profiles/benchmarks justify it; otherwise prefer simple, clear code.

Don't write sloppy code, though:
- Avoid *unnecessary* allocation and copying.
- Use iterators (they help avoid slow bounds checks too).
- Don't use `Arc`/`Mutex` in single-threaded code — use `Rc`/`RefCell`.
- Avoid global mutable state.
- In performance-sensitive code, build strings with `push`/`push_str` rather than the `format!` macro.
- Don't worry about empty `Vec`s/`String`s — they don't allocate and are essentially free.
- Consider algorithmic complexity, but weigh it against realistic input sizes.

Don't hide potentially expensive code:
- Code that may do I/O, block, or sleep should be clearly named and documented as such.
- Allocation should either be an obvious consequence of the function's purpose, or explicitly documented (e.g. a pure conversion function shouldn't silently allocate).

Concurrency design has a large impact on performance — think about whether and how code can be concurrent at design time, and design to avoid locking/synchronization in performance-sensitive paths.

**Rationale:** modern compilers/CPUs generally optimize simple code better than hand-tuned complex code (and hand-tuning is hard to predict on modern CPUs); allocation is a common source of slowdown; iterators help avoid bounds-check overhead; don't surprise callers of your code; scaling to multiple threads is what performant code needs, and blocking on locks/IO kills parallelism.
