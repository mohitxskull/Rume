# Security Review — v01

## Summary

Rume's security surface is small and the code shows deliberate intent: a `redact()` helper, a `secrecy` dependency (advertised in the `env.rs` doc example), process env hidden by default in `env:check`, and `#![forbid(unsafe_code)]` on the core crate. However, there are real, reachable leakage paths: `file:` env resolution permits path traversal (arbitrary file read), secrets live as plain `String` in memory rather than `secrecy::SecretBox`, two CLI inspection commands dump secret-bearing values to the terminal in plaintext, and `AppError` renders `to_string()` into the client-facing JSON `error` field. None of these are critical on their own (each needs a precondition), but together they mean secrets can reach the terminal, logs, HTTP clients, and the filesystem. A fix pass is required before the code handles secrets in production.

## Findings

### M1 — `file:` env resolution permits path traversal, enabling arbitrary file read

**Where:** `rume/src/env.rs` §`resolve_value`, lines 212–234; the `file:` branch at lines 218–223.

**What it says:**

```rust
if let Some(path) = value.strip_prefix("file:") {
    let full = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        dir.join(path)
    };

    let contents = fs::read_to_string(&full)?;
    ...
}
```

A `.env` value such as `APP_KEY=file:../../etc/secrets/app.key` or `APP_KEY=file:/etc/passwd` is read from disk and injected into the environment as `APP_KEY`. Relative paths are joined with `dir` but no `..` segment is rejected, and absolute paths are accepted wholesale.

**Why it's a problem:** This is a path-traversal / arbitrary-file-read primitive through `.env` files. Anyone who can write a line into a project's `.env` (a committed template, a `rume new` scaffold, a shared deploy artifact, or a malicious contributor) can cause the process to read any file it has permission to read — SSH keys, other projects' `.env`, system files — and inject the contents as an environment variable. No traversal guard or sandboxing to the project root exists.

**Suggested fix:** Reject any path containing `..` segments (after canonicalization) and reject absolute paths, or resolve and canonicalize `full` and verify it remains rooted under `dir`. Document the sandbox in the `file:` feature docs.

### M2 — Secrets held as plain `String` in `ResolvedEnv`, not `secrecy::SecretBox`

**Where:** `rume/src/env.rs` lines 29–37 (`ResolvedEnv.values: BTreeMap<String, String>`); doc example at lines 8–14 advertises `secrecy::SecretBox<String>`.

**What it says:**

```rust
pub struct ResolvedEnv {
    pub values: BTreeMap<String, String>,   // line 32
    pub sources: BTreeMap<String, String>,
    pub file_keys: BTreeSet<String>,
}
```

The `secrecy` crate is a pinned dependency and the module doc example shows `app_key: secrecy::SecretBox<String>`, but every resolved value — including `APP_KEY`, `DB_PASSWORD`, tokens — is stored as a plain `String`.

**Why it's a problem:** Plaintext secrets sit in process memory for the lifetime of the process and can surface in memory dumps, panic backtraces, debugger output, or any code path that formats the value (logs, error `to_string()`, the `env:check` dump when redaction misses). `SecretBox`/`ExposeSecret` would at least make exfiltration deliberate rather than incidental. This is a defense-in-depth gap, not a direct leak by itself, but it amplifies every other finding.

**Suggested fix:** Store secret-typed values in `secrecy::SecretBox<String>` (or a dedicated secret map) and only `expose_secret()` at the final consumption point. At minimum, segregate flagged-secret keys from general `values` so they are never accidentally formatted.

### M3 — CLI inspection commands print secrets to the terminal in plaintext

**Where:** `rume-cli/src/commands/env/check.rs` lines 36–52 (`--all` process-env dump) and `rume-cli/src/commands/config/check.rs` lines 1–4 (raw config TOML dump).

**What it says:**

`env/check.rs`:
```rust
if all {
    let mut process_only: Vec<&String> = resolved
        .values
        .keys()
        .filter(|k| !resolved.file_keys.contains(*k))
        .collect();
    ...
    println!("  {key}={display}  (process env)");
}
```

`config/check.rs`:
```rust
pub fn run() {
    match rume::config::load_raw_pretty(std::path::Path::new(".")) {
        Ok(text) => println!("{text}"),
        ...
    }
}
```

`env:check --all` prints every process-environment variable (those not sourced from `.env`) to the terminal, and `config check` prints the merged `config/*.toml` verbatim. Both apply `redact()` (env check) or no redaction at all (config check).

**Why it's a problem:** Process environment and config TOML routinely hold secrets (`APP_KEY`, `DATABASE_URL`, `AWS_SECRET_ACCESS_KEY`, private keys). Dumping them to the terminal exposes them to shoulder-surfing, terminal scrollback, and — critically — CI logs, which are retained, often shared, and sometimes public. Even with `redact()`, the heuristic has false negatives (finding m1), so secrets under non-matching key names print in full. `config check` applies no redaction at all.

**Suggested fix:** Default `env:check` and `config check` to secret-safe output: always redact secret keys and credential URLs, and for `config check` either redact known secret keys or refuse to print values for them, offering an explicit `--unredacted` (or `--dangerous`) flag for local debugging only. Never print raw config TOML to the terminal by default.

### M4 — `AppError` renders `to_string()` into the client-facing JSON `error` field

**Where:** `rume/src/error.rs` lines 56–76 (`IntoResponse`), specifically lines 68–73; `Internal(String)` at lines 34–36.

**What it says:**

```rust
_ => {
    let body = Json(ErrorBody {
        error: self.to_string(),
    });
    (status, body).into_response()
}
```

For `Internal`, `BadRequest`, `NotFound`, `Unauthorized`, and `Forbidden`, the body is `{"error": self.to_string()}` and it is sent to the HTTP client. `Validation` is the only variant that uses structured, non-`to_string()` `details` (lines 61–67).

**Why it's a problem:** Any secret or internal detail a handler places into `AppError::Internal(...)` or `AppError::BadRequest(...)` is returned verbatim to the client — DB connection strings, file paths, credentials a user submitted, internal diagnostic strings. `Internal` in particular can leak operational internals that aid further attacks. This is information leakage over the network by default.

**Suggested fix:** Never render raw `to_string()` for `Internal`/`BadRequest` to clients. Return generic, safe messages (`"internal error"`, `"bad request"`) in the response and log the detail server-side with a correlation ID; optionally include a non-sensitive detail only when explicitly opted in.

### m1 — `redact()` heuristic has false negatives: secrets under non-matching key names print plaintext

**Where:** `rume/src/env.rs` lines 237–263 (`is_secret_key`, `value_looks_sensitive`, `redact`).

**What it says:**

```rust
pub fn is_secret_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.contains("SECRET")
        || upper.contains("PASSWORD")
        || upper.contains("TOKEN")
        || upper.ends_with("_KEY")
        || upper == "KEY"
}
pub fn value_looks_sensitive(value: &str) -> bool {
    // only true for `user:pass@host` in a URL authority
    ...
}
pub fn redact(key: &str, value: &str) -> String {
    if is_secret_key(key) || value_looks_sensitive(value) {
        "[redacted]".to_string()
    } else {
        value.to_string()
    }
}
```

Redaction is key-name-based plus a URL-credential heuristic. A secret stored under a benign key name (e.g. `STRIPE_SK`, a JWT, a `-----BEGIN PRIVATE KEY-----` block under `MAIL_CONFIG`) is not redacted.

**Why it's a problem:** `env:check` (with or without `--all`) and any other caller of `redact()` print such secrets in full to the terminal. It is best-effort redaction with documented gaps, but those gaps directly produce plaintext secret disclosure in `env:check` output.

**Suggested fix:** Broaden detection (e.g. also match `_SECRET`, `_PASS`, `_PWD`, `_CRED`, `_PRIVATE_KEY`, `SK`, and common prefixes like `STRIPE_`), or move redaction to an allowlist of *known-safe* keys rather than a denylist of secret patterns, so unknown keys are redacted by default. Treat `redact()` as best-effort and document its false negatives.

### m2 — `/health/ready` exposes pid and platform in `debug_info`

**Where:** `rume/src/routing.rs` line 458 (`HealthReport.debug_info`) and lines 531–542 (`debug_info()`).

**What it says:**

```rust
fn debug_info() -> BTreeMap<String, serde_json::Value> {
    let mut info = BTreeMap::new();
    info.insert("pid".to_string(), serde_json::Value::Number(std::process::id().into()));
    info.insert("platform".to_string(), serde_json::Value::String(std::env::consts::OS.to_string()));
    info
}
```

`debug_info()` is serialized into every `/health/ready` response.

**Why it's a problem:** PID and OS are reconnaissance information: OS fingerprinting for targeted exploits, and PID can aid PID-based attacks (e.g. signal targeting, /proc inspection on the same host). It is low value to legitimate clients and unnecessary to expose.

**Suggested fix:** Remove `pid` and `platform` from `debug_info()`, or gate them behind a development-only flag so they are not emitted in production health responses.

### m3 — `Rejection` echoes parameter values and names in JSON error responses

**Where:** `rume/src/routing.rs` lines 57–81 (`Matcher::validate` builds `"{value} is not a valid ..."`) and lines 599–610 (`Rejection::IntoResponse` returns `{"error": message}`).

**What it says:**

```rust
Err(format!("{value} is not a valid number"))
Err(format!("{value} is not a valid UUID"))
Err(format!("{value} is not a valid slug"))
// ...
(status, Json(serde_json::json!({"error": message}))).into_response()
```

The raw parameter value and its name are interpolated into rejection messages and returned to the client.

**Why it's a problem:** This reflects user-supplied input back in API responses. In a JSON context XSS risk is low, but echoing values can expose sensitive data a client mistakenly placed in a URL parameter, and it aids enumeration/probing by returning the exact rejected input. Parameter names also leak route structure.

**Suggested fix:** Return generic messages that name the parameter and the expected format without echoing the raw value (e.g. `"parameter id must be a valid UUID"`), or echo only in development mode.

## Positive notes

- `redact()` is applied consistently to both `.env`-sourced and process-env values in `env:check` (`rume-cli/src/commands/env/check.rs:30,48`), and process env is hidden by default without `--all` (`check.rs:10–13,53–61`) — a good secure default.
- `value_looks_sensitive()` catches `user:pass@host` credential URLs (`env.rs:247–254`), and `is_secret_key()` covers common secret key names (`env.rs:237–244`).
- `secrecy` is a pinned dependency and the `env.rs` doc example advertises `secrecy::SecretBox<String>` (`env.rs:13`) — the right building block exists, it just isn't used for `ResolvedEnv` yet (see M2).
- `Validation` errors use structured `details` rather than raw `to_string()` (`error.rs:61–67`), avoiding the leakage pattern found in M4 for that variant.
- The core crate carries `#![forbid(unsafe_code)]` (`lib.rs:7`), reducing the memory-safety attack surface.

## Verdict

**Not ready to ship as-is for production secret handling.** The four Major findings are genuine leakage paths: arbitrary file read via `file:` path traversal (M1), plaintext secrets in memory (M2), CLI inspection dumping secrets to the terminal/CI logs (M3), and error responses echoing internals to HTTP clients (M4). Each needs a fix pass. The minor findings (m1–m3) should be closed in the same pass. The foundation is sound (redact helper, `secrecy` dependency, hidden-by-default process env, `forbid(unsafe_code)`), but the leakage paths must be closed before Rume is entrusted with secrets.
