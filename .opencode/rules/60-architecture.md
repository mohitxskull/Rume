# Rule — Architecture constraints

Hard constraints for Rume. The full reasoning and decision log live in
`docs/specs/vision/README.md`; this file records only the constraints that
every change must respect.

## Product shape

- Rume is an explicit, typed, production-ready application shell for Axum.
- Dependencies are constructed in code, not discovered by scanning.
- Configuration is typed and validated at boot, not templated.
- Transactions and lifecycle hooks are visible, not proxied.

## Refusals

- No runtime dependency-injection container.
- No classpath or module scanning.
- No hidden AOP such as `@Transactional`.
- No mandatory ORM or database.
- No JVM-style hot-reload promises.

## Configuration

- Environment (`Env`) and application config (`Config`) are separate systems.
- Environment values come from layered `.env` files and process environment.
- Application config is declarative TOML under `config/`, validated into Rust
  types at boot.
- Secrets use `secrecy` and are redacted in logs and diagnostics.

## Crate boundaries

- `rume` is the core and must not depend on heavy optional crates.
- Optional features live in `rume-*` crates that depend on `rume`.
- The CLI lives in `rume-cli`.

## Pre-v1

- The v1 contract in `docs/specs/vision/README.md` §7 is frozen.
- Everything outside it is experimental and may break.
- Borrowed ideas must earn their maintenance cost before 1.0.
