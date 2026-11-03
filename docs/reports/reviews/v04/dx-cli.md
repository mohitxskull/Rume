# CLI & Developer Experience Review — v04

## Summary

Rume's developer experience and CLI tooling have made commendable strides since v02: `rume new` scaffolds clean, compilable, portable projects depending on published releases by default; scaffolding templates perfectly align documented operational endpoints with runtime routes (`/health` resolved across `README.md.tpl`, `routes.rs.tpl`, and `handlers/health.rs.tpl`); all generators utilize idiomatic `pascal_case` type names; terminal stickers render with Unicode display width precision; and `rume list:routes` extracts runtime routes directly from Axum's application context without heuristic source parsing. All 12 CLI unit and integration tests pass cleanly (`cargo test -p rume-cli` → 10 unit, 2 integration passed in 66s).

However, the CLI and developer tooling **cannot be certified for release without an immediate fix pass**. The subsystem suffers from **1 Critical**, **5 Major**, and **4 Minor** defects: (1) `rume config check` in human-readable mode completely bypasses secret redaction for multi-line values, dumping full RSA private keys and certificates in plaintext (C1); (2) `rume config explain` dumps secrets and credential URLs in plaintext in both human and JSON modes with zero redaction or `--unredacted` protection (M1); (3) `rume doctor` port availability check reports false positive `ok: true` when port binding fails, printing `[port availability] port 3333 is in use... (ok)` (M2); (4) component generators accept digit-prefixed and non-identifier names (e.g. `2fa`, `404`), creating uncompilable project syntax (M3); (5) component generators lack transactional rollback, leaving orphaned handler files on disk upon route marker failure and permanently blocking future generation runs (M4); and (6) interactive prompt helpers swallow cancellation (`Ctrl+C`) and lack `is_terminal` gating, failing with misleading errors in non-interactive CI environments (M5).

Verification executed:
- `cargo test -p rume-cli` → **10 passed; 0 failed**
- `cargo test -p rume-cli --test generated_project` → **2 passed; 0 failed** (includes full scaffold compilation, `cargo fmt --check`, `cargo check`, `cargo test`, all 5 component generators, conflict detection, and CLI JSON inspection verification)
- `cargo clippy -p rume-cli` → **passed**
- Executed behavioral verification via isolated test harness: multi-line private key leakage in `config check`, unredacted secrets in `config explain`, port collision false positive in `doctor`, generator route failure rollback absence, and digit-name generator compilation failure.

---

## Prior decisions

Every prior finding across v01 and v02 in this lens's charter is classified below using this review's mandatory terms (**REAFFIRMED**, **CHALLENGED**, or **NOTE-NEW**).

> [!IMPORTANT]
> **No v03 lens covered CLI & Developer Experience.** This lens was omitted in the v03 review cycle and has been restored in v04 per §1.1, §3.1, and §4 (Lens 9) of `plan.md`.

### From v01 (`docs/reports/reviews/v01/dx-usability.md` & `v01/todo.md`)

- **v01 D-M1 (`AppContext::new()` takes 6 positional args of mixed types)** — **REAFFIRMED**. Verified in `rume/src/app/context.rs:82–107, 155–171` and `rume-cli/templates/src/app.rs.tpl:26–28`. The 6-arg constructor was superseded by `AppContext::builder(env, config).state(AppState).build()`, eliminating positional argument swap hazards and manual clone decisions in the template.
- **v01 D-M2 (`Rume::builder()` documented as sole entry point, but `RumeBuilder::new()` is public)** — **REAFFIRMED**. Verified in `rume/src/app/builder.rs:295, 310`. `RumeBuilder::new` is `pub(crate)`. Scaffolding and documentation exclusively standardize on `Rume::builder(ctx)`.
- **v01 D-M3 (`health_routes()` and `ops_router()` are two overlapping health-endpoint APIs)** — **REAFFIRMED**. Verified in `rume/src/app/shutdown.rs:42–46` and `rume-cli/templates/src/app.rs.tpl:5, 14`. `ops_router` is marked `#[deprecated(since = "0.2.0")]`, removed from `rume::prelude`, and `rume::routing::health_routes` is the canonical health check interface.
- **v01 D-m1 (Prelude re-exports whole third-party crates, polluting the namespace)** — **REAFFIRMED**. Verified in `rume/src/lib.rs` and `rume/src/prelude.rs`. Whole third-party crate re-exports (`axum`, `tokio`, `serde`, `tracing`) remain purged from the prelude.
- **v01 D-m2 (`rume generate handler` uses fragile string-marker codegen with silent degradation)** — **CHALLENGED (Defective remediation & lack of transactionality)**. While missing `// rume:routes` now terminates with `exit(1)` rather than continuing with a warning, the generator writes the handler file and updates `mod.rs` before checking the marker, lacking transactional rollback. Failed generation leaves orphaned files that permanently block subsequent runs. Re-reported under finding **M4**.
- **v01 D-m3 (`rume new` scaffolds a `Cargo.toml` with local path dependency and no warning)** — **REAFFIRMED**. Verified in `rume-cli/src/commands/new.rs:28–48, 126–138` and `tests/generated_project.rs:29–54`. Scaffolding defaults to portable published versions (`rume = "0.1.0"`). Local path overrides require explicit `--path` or `RUME_DEV_PATH`, accompanied by an explicit warning note.
- **v01 D-m4 (Generated code imports via `rume::axum` / `rume::serde`)** — **REAFFIRMED**. Verified in `rume-cli/templates/Cargo.toml.tpl:8–9` and `rume-cli/src/commands/generate/handler.rs:23–24`. Generated code and templates directly declare dependencies on and import from `axum` and `serde`.
- **v01 D-m5 (No machine-readable output (`--json`) for inspection commands)** — **REAFFIRMED**. Verified in `rume-cli/src/commands/doctor.rs:136–143`, `env/check.rs:98–103`, and `config/check.rs:103–115`. Structured JSON output is supported across inspection commands. (Residual flag asymmetry is evaluated under finding **m1**).

### From v02 (`docs/reports/reviews/v02/dx-tooling.md` & `v02/todo.md`)

- **v02 M1 (OpenAPI 3.1 generator discards route parameter matchers and infers schemas via substring heuristics)** — **REAFFIRMED**. Core routing and OpenAPI schema extraction is governed by Lens 8 (`routing-api-openapi.md`).
- **v02 M2 (`AppContext::builder` fails to infer state generic and requires `S: Default`)** — **REAFFIRMED**. Verified in `rume/src/app/context.rs:82–107`. `AppContext::builder(env, config)` initializes with state `()` and `.state(S)` transitions the state generic without requiring `S: Default`. Scaffolded `app.rs.tpl` correctly utilizes the builder.
- **v02 M3 (Configuration provenance reporting is entirely absent)** — **CHALLENGED (Defective remediation: unredacted secret leakage)**. While `config::source_map` and `rume config explain <key>` were added to report winning configuration files and precedence hierarchies, `rume config explain` completely omits secret redaction and provides no `--unredacted` gate, dumping raw database credentials in plaintext. Re-reported under finding **M1**.
- **v02 M4 (Environment interpolation error formatting hides variable name via escaped braces)** — **REAFFIRMED**. Verified in `rume/src/env/error.rs:23–27`. Format strings use `${{{variable}}}`, correctly displaying failing variable names in error messages.
- **v02 M5 (Scaffolding template README advertises non-existent `/health` route while `routes.rs.tpl` squats `/users` and `/posts`)** — **REAFFIRMED**. Verified in `rume-cli/templates/README.md.tpl:11`, `src/routes.rs.tpl:10–13`, `src/handlers/health.rs.tpl:18–23`, and `tests/smoke.rs.tpl:28–31`. Scaffolding registers and documents `/health` consistently. Dummy route squatting on `/users` and `/posts` was completely eliminated.
- **v02 m1 (Orphaned `ops_router` preserves duplicate health check API)** — **REAFFIRMED**. Verified: deprecated with `#[deprecated]`, removed from `prelude.rs`.
- **v02 m2 (Inconsistent JSON CLI flags across inspection commands: `--format json` vs `--json`)** — **CHALLENGED (Incomplete remediation)**. While `--json` was added to `doctor`, `env check`, and `config check`, `--format json` was never added to `list:routes`. Running `rume list:routes --format json` fails with `error: unexpected argument '--format' found`. Re-reported under finding **m1**.
- **v02 m3 (CLI generators use naive first-letter `capitalize` rather than `rume::helpers::pascal_case`)** — **REAFFIRMED**. Verified in `rume-cli/src/commands/generate/resource.rs:10`, `handler.rs:14`, `model.rs:16`, and `service.rs:16`. All generators call `rume::helpers::pascal_case(&module_name)`, verified by unit test `module_name_and_struct_name_derivation`.
- **v02 m4 (`RouteGroup` lacks fluent `.layer()` middleware mounting method)** — **REAFFIRMED**. Governed by Lens 8.
- **v02 m5 (`RumeBuilder` lacks fluent `.openapi()` and `.metrics()` methods)** — **REAFFIRMED**. Evaluated under Lens 7 and Lens 8.
- **v02 m6 (Stale "extractor" docstrings and unused phantom type parameter on `ValidatedParam`)** — **REAFFIRMED**. Governed by Lens 8.
- **v02 m7 (Generated project `tests/smoke.rs.tpl` tests throwaway inline router)** — **REAFFIRMED**. Verified in `rume-cli/templates/tests/smoke.rs.tpl:25–29`. Smoke test builds the actual application router via `build_router(ctx.clone()).with_state(ctx)` and executes tests against `TestApp`.
- **v02 m8 (Terminal UI `Sticker` calculates box padding using byte length instead of display width)** — **REAFFIRMED**. Verified in `rume-cli/src/ui/sticker.rs:29–37`. `Sticker` uses `unicode_width::UnicodeWidthStr::width`, verified by unit test `sticker_aligns_with_unicode_characters` covering emojis and multi-byte characters.

---

## Findings

### C1 Multi-line secret leakage in `rume config check` human-readable mode

- **Where:** `rume-cli/src/commands/config/check.rs:29–51, 117–127`.
- **What it says:**
  ```rust
  fn redact_toml_line(line: &str) -> String {
      if let Some((key, rest)) = line.split_once('=') {
          let key = key.trim();
          let trimmed = rest.trim();
          let value = match toml_value_inner(trimmed) {
              Some(v) => v,
              None => return line.to_string(),
          };
          if redact_value(key, &value) == "[redacted]" {
              if trimmed.starts_with('"') && trimmed.ends_with('"') {
                  return format!("{key} = \"[redacted]\"");
              }
              ...
          }
      }
      line.to_string()
  }

  pub fn run(unredacted: bool, format: &str) {
      match config::load_raw(std::path::Path::new(".")) {
          Ok(value) => match format {
              "json" => {
                  let mut output_value = value.clone();
                  if !unredacted {
                      redact_toml_value(&mut output_value);
                  }
                  ...
              }
              _ => {
                  let text = toml::to_string_pretty(&value).unwrap_or_default();
                  if unredacted {
                      println!("{text}");
                  } else {
                      let redacted: String = text
                          .lines()
                          .map(redact_toml_line)
                          .collect::<Vec<_>>()
                          .join("\n");
                      println!("{redacted}");
                  }
              }
          }
  ```
- **Why it's a problem:**
  In JSON mode, `config check` operates on the structured `toml::Value` AST via `redact_toml_value(&mut output_value)`. However, in human-readable mode (the default mode), it converts the raw `value` (which contains unredacted secrets) into a formatted TOML string and naively filters it line-by-line via `redact_toml_line`.

  `redact_toml_line` checks `if let Some((key, rest)) = line.split_once('=')`. If an application configuration contains a multi-line secret string (such as an RSA/ECDSA private key, a SAML signing key, an X.509 certificate, a multi-line JWT token, or an SSH key):
  ```toml
  [auth]
  private_key = """
  -----BEGIN RSA PRIVATE KEY-----
  MIIEowIBAAKCAQEA0Y1234567890SECRETKEYDATA...
  -----END RSA PRIVATE KEY-----"""
  ```
  Only the first line (`private_key = """`) contains an `=` delimiter. `redact_toml_line` replaces the first line with `private_key = "[redacted]"`. All subsequent lines containing the actual cryptographic key material lack an `=` sign, causing `redact_toml_line` to execute `return line.to_string()`.

  **Executed verification:** In an isolated test project with an RSA private key configured under `[auth]`:
  ```sh
  rume config check
  ```
  Output produced:
  ```toml
  [auth]
  private_key = "[redacted]"
  -----BEGIN RSA PRIVATE KEY-----
  MIIEowIBAAKCAQEA0Y1234567890SECRETKEYDATA
  abcdefghijklmnopqrstuvwxyz1234567890
  -----END RSA PRIVATE KEY-----"""
  ```
  The full cryptographic private key is leaked in plaintext to stdout and CI logs.
- **Suggested fix:**
  Eliminate `redact_toml_line` entirely. In human-readable mode, apply `redact_toml_value(&mut output_value)` directly to the `toml::Value` AST before string formatting, identical to JSON mode:
  ```rust
  let mut output_value = value.clone();
  if !unredacted {
      redact_toml_value(&mut output_value);
  }
  let text = toml::to_string_pretty(&output_value).unwrap_or_default();
  println!("{text}");
  ```

---

### M1 `rume config explain` outputs secret keys and credential URLs in plaintext without redaction or `--unredacted` gating

- **Where:** `rume-cli/src/commands/config/explain.rs:20–42` and `rume-cli/src/cli.rs:110–117`.
- **What it says:**
  In `rume-cli/src/commands/config/explain.rs`:
  ```rust
  let explanation = source_map.explain(key);
  match &explanation.value {
      Some(val) => {
          if json {
              println!(
                  "{}",
                  serde_json::to_string_pretty(&explanation).unwrap_or_default()
              );
          } else {
              println!("KEY: {}", explanation.key);
              println!("Resolved value: {val}");
              ...
          }
      }
  ```
  In `rume-cli/src/cli.rs`:
  ```rust
  /// Explain how a configuration key was resolved and its source provenance.
  Explain {
      /// Config key to explain (e.g. app.name or database.url).
      key: String,
      /// Output as JSON.
      #[arg(long)]
      json: bool,
  },
  ```
- **Why it's a problem:**
  `rume config explain <key>` was introduced to fulfill v02 finding M3 (configuration provenance reporting). However, unlike `rume env explain` (which calls `is_secret_key` and `redact_value`) and `rume config check` (which redacts secrets by default), `rume config explain` performs **no secret redaction whatsoever**.

  Furthermore, `rume config explain` does not declare an `--unredacted` CLI flag: it unconditionally prints the raw value of any requested configuration key in both human-readable and JSON modes. The CLI help text explicitly prompts developers to run `rume config explain database.url`.

  **Executed verification:** In an isolated test project with `database.password = "super_secret_db_pass"` and `database.url = "postgres://admin:secret123@localhost/db"`:
  ```sh
  rume config explain database.password
  ```
  Output produced:
  ```text
  KEY: database.password
  Resolved value: "super_secret_db_pass"
  Source: ./config/app.toml
  Precedence hierarchy:
    - ./config/app.toml
  ```
  And in JSON mode (`rume config explain database.password --json`):
  ```json
  {
    "key": "database.password",
    "value": "super_secret_db_pass",
    "winning_file": "./config/app.toml",
    "candidate_precedence": [
      "./config/app.toml"
    ]
  }
  ```
  Any developer, script, or CI pipeline querying configuration provenance exposes passwords, API keys, and database connection strings in plaintext.
- **Suggested fix:**
  1. Add an `#[arg(long)] unredacted: bool` flag to `ConfigCommands::Explain` in `rume-cli/src/cli.rs`.
  2. In `rume-cli/src/commands/config/explain.rs`, if `!unredacted`, check `is_secret_key(key)` or serialize the value to string and run `redact_value(key, &val_str)`. If redacted, replace `val` with `toml::Value::String("[redacted]".to_string())`.
  3. Mirror `rume env explain` by emitting a note in human mode: `"Note: this value is treated as a secret and is redacted in output. Use --unredacted to display."`.

---

### M2 `rume doctor` port availability check reports false positive `ok: true` when port binding fails

- **Where:** `rume-cli/src/commands/doctor.rs:110–128, 149–153`.
- **What it says:**
  ```rust
  match std::net::TcpListener::bind((host, port)) {
      Ok(_) => format!("port {port} is available"),
      Err(err) => {
          ok = false;
          format!("port {port} is in use: {err} (stop the process or change PORT)")
      }
  }
  let port_ok = port_result.starts_with("port ");
  checks.push(DoctorCheck {
      label: "port availability".to_string(),
      status: port_result,
      ok: port_ok,
  });
  ```
- **Why it's a problem:**
  When `TcpListener::bind` fails, line 113 correctly sets the overall accumulator `ok = false;`. However, line 114 formats `port_result` as `"port {port} is in use: ..."` and line 123 evaluates `let port_ok = port_result.starts_with("port ");`.

  Because `"port 3333 is in use..."` starts with `"port "`, `port_ok` unconditionally evaluates to `true`.

  **Executed verification:** With an active listener bound to port 3333:
  1. Human output:
     ```text
     [port availability] port 3333 is in use: Address already in use (os error 98) (stop the process or change PORT) (ok)
     ```
     The check status displays green `(ok)` directly alongside the error message.
  2. JSON output (`rume doctor --json`):
     ```json
     {
       "all_passed": false,
       "checks": [
         {
           "label": "port availability",
           "status": "port 3333 is in use: Address already in use (os error 98) (stop the process or change PORT)",
           "ok": true
         }
       ]
     }
     ```
  Every single check in `checks` has `"ok": true`. Monitoring systems or IDE plugins evaluating the `port availability` check receive a false-positive success signal, violating Charter 2 ("honest diagnostics: no false positives masking broken environments").
- **Suggested fix:**
  Track the boolean result of `bind` directly instead of matching on strings:
  ```rust
  let (port_ok, port_result) = match std::net::TcpListener::bind((host, port)) {
      Ok(_) => (true, format!("port {port} is available")),
      Err(err) => (false, format!("port {port} is in use: {err} (stop the process or change PORT)")),
  };
  if !port_ok {
      ok = false;
  }
  checks.push(DoctorCheck {
      label: "port availability".to_string(),
      status: port_result,
      ok: port_ok,
  });
  ```

---

### M3 Component generators accept digit-prefixed and non-identifier names, creating uncompilable project syntax

- **Where:** `rume-cli/src/commands/generate/handler.rs:4–11`, `resource.rs:185–190`, `model.rs:6–13`, `service.rs:6–13`, `middleware.rs:6–13`.
- **What it says:**
  In `handler.rs`:
  ```rust
  if name.is_empty()
      || !name
          .chars()
          .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
  {
      eprintln!("error: handler name must contain only letters, numbers, '-' or '_'");
      std::process::exit(1);
  }

  let module_name = name.to_ascii_lowercase().replace('-', "_");
  let struct_name = rume::helpers::pascal_case(&module_name);
  ```
- **Why it's a problem:**
  The validation check permits any alphanumeric string, including names starting with digits (e.g. `2fa`, `404`, `0auth`, `3d_model`) and reserved Rust keywords (e.g. `type`, `match`, `yield`).

  In Rust, identifiers (module names, struct names) **cannot start with an ASCII digit**. When a developer runs `rume generate handler 2fa`:
  1. `module_name` becomes `2fa`.
  2. `src/handlers/2fa.rs` is created.
  3. `pub mod 2fa;` is inserted into `src/handlers/mod.rs`.
  4. `pub struct 2faResponse` is written.

  **Executed verification:** Running `rustc` on the generated module declaration:
  ```text
  error: expected identifier, found `2fa`
   --> src/handlers/mod.rs:2:9
    |
  2 | pub mod 2fa;
    |         ^^^ expected identifier
    |
  help: identifiers cannot start with a number
  ```
  `cargo check` immediately fails across the entire project. The user's build is completely broken until they manually delete the file, clean `mod.rs`, and edit `routes.rs`.
- **Suggested fix:**
  In all generators (and `new.rs`), enforce that the derived `module_name` is a valid Rust identifier:
  1. The first character must satisfy `c.is_ascii_alphabetic() || c == '_'`.
  2. The name must not match reserved Rust keywords (`type`, `fn`, `struct`, `impl`, `match`, `enum`, `mod`, `let`, etc.).

---

### M4 Non-transactional component generation: route marker failure leaves orphaned handler files and blocks future runs

- **Where:** `rume-cli/src/commands/generate/handler.rs:38–86`, `resource.rs:92–171`.
- **What it says:**
  In `handler.rs`:
  ```rust
  // Step 1: Write handler file
  if let Err(err) = fs::write(&handler_path, content) { ... }

  // Step 2: Register module in mod.rs
  match fs::read_to_string(mod_path) {
      Ok(existing) if !existing.contains(&mod_entry) => {
          fs::write(mod_path, format!("{existing}{mod_entry}")) ...
      }
      ...
  }

  // Step 3: Register route in routes.rs
  match fs::read_to_string(routes_path) {
      Ok(existing) => {
          if let Some(marker) = existing.find("    // rume:routes") {
              fs::write(routes_path, updated) ...
          } else {
              eprintln!("error: could not find route marker `// rume:routes` in {routes_path}...");
              std::process::exit(1);
          }
      }
  }
  ```
- **Why it's a problem:**
  Generation is non-transactional and lacks rollback. If `src/routes.rs` is missing `// rume:routes` (e.g. removed or reformatted by the developer) or suffers an I/O error:
  1. `src/handlers/{name}.rs` has already been written to disk.
  2. `src/handlers/mod.rs` has already been modified on disk.
  3. The process exits with code 1.

  **Executed verification:** In an isolated test project with `// rume:routes` removed:
  ```sh
  rume generate handler testroute
  # Fails with: error: could not find route marker `// rume:routes`
  rume generate handler testroute
  # Fails with: error: handler `testroute` already exists
  ```
  Because the handler file was created in the first run and not rolled back, subsequent runs fail at line 18 (`handler already exists`). The developer is permanently blocked until they manually find and delete the orphaned file and edit `mod.rs`. Contrast with `rume new` (`new.rs:172–178`), which properly performs teardown cleanup on failure.
- **Suggested fix:**
  1. Validate pre-conditions first: check that `src/routes.rs` exists and contains the route marker *before* creating any files on disk.
  2. If any subsequent write fails, roll back created files (`fs::remove_file(&handler_path)`).

---

### M5 Interactive prompts in `prompts.rs` swallow cancellation (Ctrl+C) and fail silently in non-TTY environments

- **Where:** `rume-cli/src/prompts.rs:9–48`, `rume-cli/src/commands/generate/mod.rs:25–35`, `rume-cli/src/commands/mod.rs:41–48`.
- **What it says:**
  In `rume-cli/src/prompts.rs`:
  ```rust
  pub fn ask(prompt: &str) -> String {
      Text::new(prompt).prompt().unwrap_or_default()
  }

  pub fn ask_default(prompt: &str, default: &str) -> String {
      Text::new(prompt)
          .with_default(default)
          .prompt()
          .unwrap_or_else(|_| default.to_string())
  }
  ```
- **Why it's a problem:**
  1. **Swallowed Cancellation:** When a developer runs `rume new` or `rume generate handler` without arguments and presses `Ctrl+C` or `Esc` to cancel, Inquire returns `Err(InquireError::OperationCanceled)` or `Err(InquireError::OperationInterrupted)`. `ask()` catches the error and silently returns `""`. The caller receives an empty string and reports `"error: project name is required"` with exit code 1. Instead of aborting cleanly on SIGINT (exit code 130), the CLI treats user cancellation as a validation failure.
  2. **Missing `is_terminal` Gating:** There is no check for `std::io::stdin().is_terminal()`. If `rume new` or `rume generate` is executed in a headless CI/CD pipeline or non-interactive script where stdin is closed or redirected (`/dev/null`), Inquire fails with `InquireError::NotTTY`. `prompts.rs` swallows `NotTTY`, returns `""`, and reports `"error: name is required"`. The error gives no indication that the environment is non-interactive.
- **Suggested fix:**
  In `prompts.rs`:
  1. Check `std::io::stdin().is_terminal()`. If false, fail immediately with an actionable error: `"error: standard input is not a terminal; specify the argument directly"`.
  2. Propagate `Result<String, InquireError>` or handle `InquireError::OperationCanceled` / `OperationInterrupted` by exiting immediately with status 130 (`std::process::exit(130)`).

---

### m1 Inconsistent CLI argument flags for JSON output (`--format json` fails on `list:routes`)

- **Where:** `rume-cli/src/cli.rs:48–54` and `rume-cli/src/commands/list_routes.rs:16–35`.
- **What it says:**
  In `rume-cli/src/cli.rs`:
  ```rust
  Doctor {
      #[arg(long, default_value = "human")]
      format: String,
      #[arg(long)]
      json: bool,
  },
  ListRoutes {
      #[arg(long)]
      json: bool,
  },
  ```
- **Why it's a problem:**
  In v02 finding m2, flag inconsistency was reported across inspection commands. Remediation added `--json` to `doctor`, `env check`, and `config check`, but never added `--format` to `list:routes`.

  **Executed verification:**
  ```sh
  rume list:routes --format json
  ```
  Fails with: `error: unexpected argument '--format' found`. CI scripts and tooling attempting to standardize on `--format json` across all Rume inspection commands break when calling `list:routes`.
  Furthermore, if `list:routes` encounters a build failure or missing route metadata, it emits plaintext stderr rather than a structured JSON error object `{"error": ...}`, unlike `env check` and `config check`.
- **Suggested fix:**
  Add `#[arg(long, default_value = "human")] format: String` to `Commands::ListRoutes` in `cli.rs`, and output structured JSON errors on failure when `--json` or `--format json` is active.

---

### m2 Unused and dead UI code with crate-level `#![allow(dead_code)]`

- **Where:** `rume-cli/src/main.rs:1`, `rume-cli/src/ui/logger.rs`, `rume-cli/src/ui/tasks.rs`, `rume-cli/src/prompts.rs:14–48`.
- **What it says:**
  `main.rs` opens with `#![allow(dead_code)]`.
  Entire modules exist in isolation and are never referenced anywhere in `rume-cli`:
  - `ui/logger.rs` (118 lines, implementing `Logger`, `Level`, terminal log formatting)
  - `ui/tasks.rs` (67 lines, implementing `TaskRunner`, `TaskHandle`, spinners)
  - `prompts.rs` functions `ask_default`, `secure`, `confirm`, `select`, `multi_select`.
- **Why it's a problem:**
  Dead code bloats the binary and gives a false impression of CLI capability. The crate-level `#![allow(dead_code)]` suppresses compiler warnings that would otherwise prevent regressions and dead code accumulation.
- **Suggested fix:**
  Remove `#![allow(dead_code)]` from `main.rs`. Either integrate `Logger` and `TaskRunner` into existing CLI commands (e.g. multi-step scaffolding or doctor checks), or delete the orphaned code.

---

### m3 Generated project test template `tests/smoke.rs.tpl` forces `unsafe` blocks for environment loading

- **Where:** `rume-cli/templates/tests/smoke.rs.tpl:10–12, 19–21`.
- **What it says:**
  ```rust
  #[test]
  fn config_loads() {
      // Ensure test-specific config is used.
      unsafe {
          std::env::set_var("RUME_ENV", "test");
      }
      let config: Config = rume::config::load().expect("config should load");
      assert!(!config.app.name.is_empty());
  }
  ```
- **Why it's a problem:**
  In Rust 2024 (`edition = "2024"` in `Cargo.toml.tpl`), `std::env::set_var` is an `unsafe fn` due to POSIX thread-safety data races. Scaffolding starter projects with `unsafe` blocks in day-one test code teaches anti-patterns, fails in projects enforcing `#![forbid(unsafe_code)]`, and introduces test race conditions when running tests in parallel threads with `cargo test`.
- **Suggested fix:**
  Provide safe, test-isolated configuration loading in `rume::config` (e.g. `rume::config::load_for_env("test")` or `AppContext::builder(env, config)` without mutating process-global environment variables).

---

### m4 `rume codegen` lacks dry-run/diff support and blindly includes invalid module files

- **Where:** `rume-cli/src/commands/codegen/mod.rs:38–81`.
- **What it says:**
  ```rust
  for entry in entries.flatten() {
      let path = entry.path();
      if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
          continue;
      }
      let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
          continue;
      };
      if stem == "mod" {
          continue;
      }
      modules.push(stem.to_string());
  }
  ...
  fs::write(&mod_path, content)
  ```
- **Why it's a problem:**
  1. `rume codegen` provides no `--dry-run` or `--diff` flag, unconditionally overwriting `mod.rs`. Any custom comments, docstrings, or module attributes in `mod.rs` are destroyed.
  2. It does not validate that `stem` is a valid Rust identifier. If a non-code file or temporary file (e.g. `foo-bar.rs`, `404.rs`) exists in `src/handlers/`, `codegen` writes `pub mod foo-bar;` or `pub mod 404;`, which causes immediate compilation failure.
- **Suggested fix:**
  Filter entries through a Rust identifier validator (`stem.chars().next().map(|c| c.is_ascii_alphabetic() || c == '_')`), and add a `--dry-run` flag to display proposed changes before writing to disk.

---

## Positive notes

- **Portable Scaffolding by Default (`rume new`)**: Projects scaffold cleanly with released crate versions (`rume = "0.1.0"`), with local path dependencies reserved for explicit development flags accompanied by helpful user advice.
- **Seamless Milestone 2 Integration Gate**: `tests/generated_project.rs` provides an end-to-end integration test verifying that scaffolded projects compile, pass `cargo fmt --check`, pass `cargo check`, pass `cargo test`, run all 5 generators, catch duplicate component conflicts, and produce valid machine-readable JSON across inspection commands.
- **Zero-Parsing Runtime Route Inspection (`list:routes`)**: By executing the application under `ApplicationMode::Inspect` and serializing Axum's live `RouteRegistry`, `list:routes` reflects actual runtime routes with 100% fidelity without brittle regex/AST source parsing.
- **High-Fidelity Unicode Terminal UI**: Terminal stickers utilize `unicode_width` to ensure flawless visual border alignment across emojis and non-ASCII text.
- **Clean Public-Only Core Coupling**: The CLI strictly interfaces with `rume` through public API surfaces (`rume::config`, `rume::env`, `rume::routing::RouteInfo`, `rume::helpers::pascal_case`), avoiding private backdoor dependencies.

---

## Verdict

**Not ready to proceed without a fix pass.**

While Rume v04 resolves prior scaffolding and template alignment defects (M5, D-M1, D-M2, m7, m8), **1 Critical finding** (C1: multi-line private key leakage in `rume config check`) and **5 Major findings** (M1: unredacted secrets in `config explain`; M2: false-positive `ok: true` on port collisions in `doctor`; M3: syntax-breaking digit names in generators; M4: non-transactional generator failure leaving orphaned files; M5: cancellation and non-TTY prompt errors) undermine production safety, security, and developer confidence.

Conditions to pass:
1. Fix **C1** by applying `redact_toml_value` to the `toml::Value` AST before string formatting in `config check`, eliminating multi-line secret leaks.
2. Fix **M1** by gating `config explain` behind secret redaction by default and introducing an `--unredacted` CLI flag.
3. Fix **M2** by evaluating the boolean result of port binding in `doctor.rs` rather than checking `starts_with("port ")`.
4. Fix **M3** and **M4** by validating Rust identifier names and ensuring transactional rollback in generators.
5. Fix **M5** by adding `is_terminal` gating and clean SIGINT handling in `prompts.rs`.
