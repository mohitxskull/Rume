# Rume

Rume is an explicit, typed, production-ready application shell for [Axum](https://github.com/tokio-rs/axum).

> ⚠️ **Work in progress — not stable.** Rume is pre-1.0. The public API, configuration shape, and behavioral contracts may change between releases without notice. Do not depend on it for production workloads yet.

## What Rume is

Rume is an application shell, not a magic runtime container. It provides:

- **Progressive complexity** — start with a 3-line stateless app, add modules when you need them
- **Explicit by construction** — dependencies, transactions, and lifecycle hooks are visible in plain Rust code
- **First-class modules** — bounded domains with encapsulated state, explicit dependencies, and topological lifecycle
- **No hidden DI container** — no classpath scanning, no `@Transactional`-style AOP, no runtime magic
- **Typed config validated at boot** — declarative TOML under `config/`, validated into Rust types
- **Secrets with `secrecy`** — redacted in logs and diagnostics

## Quick start

```rust
use rume::Rume;

#[tokio::main]
async fn main() -> Result<(), std::io::Error> {
    Rume::new()
        .get("/", || async { "Hello, Rume!" })
        .run()
        .await
}
```

## Project layout

| Path | Purpose |
|---|---|
| `rume/` | Core framework crate |
| `rume-cli/` | CLI for scaffolding, config, routes, DB |
| `rume-macros/` | Procedural macros |
| `workbench/multi-instance/` | Reference multi-instance application + Compose qualification tests |
| `workbench/simple-api/` | Simple API reference app |
| `workbench/modular-monolith/` | Modular monolith reference app |
| `docs/specs/vision/README.md` | Product vision & architectural principles |
| `docs/specs/vision/ROADMAP.md` | Phased engineering roadmap |

## Testing

```bash
# Full check: fmt + clippy + build + test
cargo fmt --all --check && cargo clippy --workspace --all-targets && cargo test --workspace --all-targets
```

CI runs `fmt`, `clippy`, per-crate `test` with Postgres/Redis service containers, and a separate Docker Compose multi-instance qualification stage (`.github/workflows/ci.yml`). CodeQL analysis runs on push/PR/schedule (`.github/workflows/codeql.yml`).

## Documentation

- [Vision & principles](docs/specs/vision/README.md)
- [Engineering roadmap](docs/specs/vision/ROADMAP.md)

## Status

Pre-1.0. Everything outside the v1 contract in `docs/specs/vision/README.md` §7 is experimental and may break.

## License

MIT OR Apache-2.0
