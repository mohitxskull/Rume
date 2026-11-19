# {{name}}

A Rume application.

## Run

```sh
cargo run
```

Then visit http://127.0.0.1:3333/health.

For auto-reload during development, install cargo-watch and run:

```sh
cargo install cargo-watch
cargo watch -x run
```

## Environment

Copy `.env.example` to `.env` and adjust as needed.

## SQL Check & Dialects

This project was scaffolded with `{{dialect}}` placeholders in `sql/items/*.sql`. Validate SQL syntax and placeholder compatibility at any time:

```sh
rume sql:check --dialect {{dialect}}
# Or check explicitly against another target dialect:
rume sql:check --dialect sqlite
rume sql:check --dialect postgres
rume sql:check --dialect mysql
```

For multi-dialect applications, organize dialect-specific queries under subdirectories (`sql/postgres/...`, `sql/sqlite/...`, `sql/mysql/...`), which `rume::sql_assets!` binds under dialect-scoped modules (`queries::postgres::...`, `queries::sqlite::...`, `queries::mysql::...`).
