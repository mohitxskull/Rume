//! Scans SQL files in a directory and validates syntax, non-emptiness, balanced delimiters, and placeholders.
//!
//! All SQL quoting, comment, and placeholder handling is delegated to `sqlparser-rs` instead of a
//! hand-rolled character scanner (RC-12 audit finding). `sqlparser-rs` natively handles block
//! comments (`/* ... */`), line comments (`--`), `#` comments, and dialect-aware placeholders
//! (`$N` for PostgreSQL, `?` for SQLite/MySQL).

use crate::ui::table::TableBuilder;
use console::style;
use serde::Serialize;
use sqlparser::dialect::{Dialect, GenericDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Token, Tokenizer};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize)]
pub struct SqlCheckReport {
    pub all_passed: bool,
    pub total_files: usize,
    pub valid_files: usize,
    pub files: Vec<SqlFileStatus>,
}

#[derive(Serialize)]
pub struct SqlFileStatus {
    pub path: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub async fn run(
    path_str: &str,
    format: &str,
    is_json: bool,
    target_dialect: Option<&str>,
    db_dry_run: bool,
) {
    let target_path = Path::new(path_str);
    let files = match collect_target_sql_files(target_path) {
        Ok(files) => files,
        Err(err) => {
            eprintln!("{}", style(format!("error: {err}")).red());
            std::process::exit(1);
        }
    };

    let mut db_handle = None;
    if db_dry_run {
        match crate::commands::db::resolve_db_url() {
            Ok(Some(url)) => match crate::commands::db::connect_backend(&url).await {
                Ok((handle, dialect)) => {
                    // Why: Postgres and MySQL backends use `connect_lazy`, so establishing
                    // the pool alone does not verify connectivity. Ping up front so `--db`
                    // fails closed on an unreachable database even when every `.sql` file
                    // is parameterized and skips `EXPLAIN`.
                    use rume::persistence::DatabaseBackend;
                    let ping_res = match &handle {
                        crate::commands::db::BackendHandle::Postgres(b) => b.ping().await,
                        crate::commands::db::BackendHandle::Sqlite(b) => b.ping().await,
                        crate::commands::db::BackendHandle::Mysql(b) => b.ping().await,
                    };
                    if let Err(err) = ping_res {
                        eprintln!(
                            "{}",
                            style(format!("error: database unreachable for --db check: {err}"))
                                .red()
                        );
                        std::process::exit(1);
                    }
                    db_handle = Some((handle, dialect));
                }
                Err(err) => {
                    // Why: `--db` explicitly requests live-DB verification; fail closed
                    // instead of silently skipping when the database is unreachable.
                    eprintln!(
                        "{}",
                        style(format!(
                            "error: failed to connect to database for --db check: {err}"
                        ))
                        .red()
                    );
                    std::process::exit(1);
                }
            },
            Ok(None) => {
                eprintln!(
                    "{}",
                    style("error: --db requires DATABASE_URL to be configured").red()
                );
                std::process::exit(1);
            }
            Err(err) => {
                eprintln!("{}", style(format!("error: {err}")).red());
                std::process::exit(1);
            }
        }
    }

    let mut statuses = Vec::with_capacity(files.len());
    let mut all_ok = true;
    let mut valid_count = 0;

    let effective_target_dialect =
        resolve_target_dialect(target_dialect, db_handle.as_ref().map(|(_, d)| *d));

    for file_path in &files {
        let display_path = file_path.display().to_string();
        let val_res = validate_sql_file(file_path, effective_target_dialect);
        let final_res = match val_res {
            Ok((statements, _tokens, _effective_dialect)) => {
                if let Some((handle, _db_dialect)) = &db_handle {
                    // Why: inspect the parsed `sqlparser` AST instead of raw string
                    // `first_word`. This both handles leading comments (`--` / `/* */`)
                    // and guarantees multi-statement SQL (e.g. `SELECT 1; DROP TABLE ...`)
                    // or parameterized queries are never sent to `EXPLAIN`.
                    if is_safe_explainable_dml(&statements) {
                        let explain_sql = format!("EXPLAIN {}", statements[0]);
                        use rume::persistence::DatabaseBackend;
                        let explain_res = match handle {
                            crate::commands::db::BackendHandle::Postgres(b) => {
                                b.execute(&explain_sql).await
                            }
                            crate::commands::db::BackendHandle::Sqlite(b) => {
                                b.execute(&explain_sql).await
                            }
                            crate::commands::db::BackendHandle::Mysql(b) => {
                                b.execute(&explain_sql).await
                            }
                        };
                        match explain_res {
                            Ok(_) => Ok(()),
                            Err(e) => Err(format!("live DB dry-run failed: {e}")),
                        }
                    } else {
                        Ok(())
                    }
                } else {
                    Ok(())
                }
            }
            Err(e) => Err(e),
        };

        match final_res {
            Ok(()) => {
                valid_count += 1;
                statuses.push(SqlFileStatus {
                    path: display_path,
                    ok: true,
                    error: None,
                });
            }
            Err(err) => {
                all_ok = false;
                statuses.push(SqlFileStatus {
                    path: display_path,
                    ok: false,
                    error: Some(err),
                });
            }
        }
    }

    let report = SqlCheckReport {
        all_passed: all_ok,
        total_files: files.len(),
        valid_files: valid_count,
        files: statuses,
    };

    if is_json || format == "json" {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
        if !all_ok {
            std::process::exit(1);
        }
        return;
    }

    println!("Checking SQL files in `{path_str}`:");
    println!();

    if files.is_empty() {
        println!("  No SQL files found in `{path_str}`.");
        return;
    }

    let mut table = TableBuilder::new();
    table.head(&["File", "Status", "Details"]);

    for item in &report.files {
        if item.ok {
            table.row_colored(&[
                (&item.path, None),
                ("VALID", Some(comfy_table::Color::Green)),
                ("syntax ok", None),
            ]);
        } else {
            let err_msg = item.error.as_deref().unwrap_or("unknown error");
            table.row_colored(&[
                (&item.path, None),
                ("FAIL", Some(comfy_table::Color::Red)),
                (err_msg, Some(comfy_table::Color::Red)),
            ]);
        }
    }

    table.render();
    println!();
    if all_ok {
        println!(
            "{}",
            style(format!(
                "All {} SQL file(s) passed validation.",
                files.len()
            ))
            .green()
            .bold()
        );
    } else {
        println!(
            "{}",
            style(format!(
                "{}/{} SQL file(s) failed validation.",
                files.len() - valid_count,
                files.len()
            ))
            .red()
            .bold()
        );
        std::process::exit(1);
    }
}

fn resolve_target_dialect<'a>(
    cli_dialect: Option<&'a str>,
    db_dialect: Option<&'a str>,
) -> Option<&'a str> {
    cli_dialect.or(db_dialect)
}

fn collect_target_sql_files(target: &Path) -> Result<Vec<PathBuf>, String> {
    if !target.exists() {
        return Err(format!(
            "SQL target path '{}' does not exist",
            target.display()
        ));
    }
    if target.is_file() {
        return Ok(vec![target.to_path_buf()]);
    }
    let mut files = Vec::new();
    collect_sql_files(target, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_sql_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    // Why: `walkdir::WalkDir` with `follow_links(false)` avoids symlink cycles cleanly
    // without a hand-rolled recursive `fs::read_dir` walker (`RC-20`), and returning
    // traversal errors prevents silently skipping unreadable directories (`RC-42`).
    for entry_res in walkdir::WalkDir::new(dir).follow_links(false) {
        let entry =
            entry_res.map_err(|err| format!("failed to traverse {}: {err}", dir.display()))?;
        if entry.file_type().is_file()
            && entry.path().extension().and_then(|s| s.to_str()) == Some("sql")
        {
            files.push(entry.into_path());
        }
    }
    Ok(())
}

/// Returns `true` only when `statements` contains exactly one zero-parameter
/// `SELECT`/`INSERT`/`UPDATE`/`DELETE` statement safe to wrap in `EXPLAIN`.
fn is_safe_explainable_dml(statements: &[sqlparser::ast::Statement]) -> bool {
    if statements.len() != 1 {
        return false;
    }
    // Why: use `Visitor::pre_visit_value` rather than `visit_expressions` or token
    // scanning so placeholders stored outside `Expr::Value` (such as MySQL
    // `MatchAgainst.match_value` in `MATCH(body) AGAINST (?)`) and SQLite named
    // parameters (`SELECT :id;`) are all excluded from `EXPLAIN`, while PostgreSQL
    // JSONB operators (`payload ? 'active'`, parsed as a binary op) remain eligible.
    struct PlaceholderVisitor;
    impl sqlparser::ast::Visitor for PlaceholderVisitor {
        type Break = ();

        fn pre_visit_value(
            &mut self,
            value: &sqlparser::ast::ValueWithSpan,
        ) -> std::ops::ControlFlow<Self::Break> {
            if matches!(value.value, sqlparser::ast::Value::Placeholder(_)) {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        }
    }

    let has_placeholder =
        sqlparser::ast::Visit::visit(&statements[0], &mut PlaceholderVisitor).is_break();
    if has_placeholder {
        return false;
    }
    matches!(
        statements[0],
        sqlparser::ast::Statement::Query(_)
            | sqlparser::ast::Statement::Insert(_)
            | sqlparser::ast::Statement::Update { .. }
            | sqlparser::ast::Statement::Delete(_)
    )
}

type ValidatedSql<'a> = (Vec<sqlparser::ast::Statement>, Vec<Token>, Option<&'a str>);

/// Validate a single SQL file using `sqlparser-rs`.
///
/// Performs comment-aware syntax parsing, an emptiness check, and dialect-aware placeholder
/// validation. Comments (`--`, `#`, `/* */`) are tokenized as [`Token::Whitespace`] variants by
/// `sqlparser-rs`, so placeholders appearing inside comments are never flagged.
fn validate_sql_file<'a>(
    path: &Path,
    target_dialect: Option<&'a str>,
) -> Result<ValidatedSql<'a>, String> {
    let content = fs::read_to_string(path).map_err(|e| format!("cannot read file: {e}"))?;

    // Resolve the effective dialect: an explicit `--dialect` flag wins, otherwise infer from the
    // file path, falling back to GenericDialect when neither is available.
    // Why: the dialect drives both syntax parsing and the per-dialect placeholder rules.
    let effective_dialect = target_dialect.or_else(|| infer_dialect_from_path(path));
    let dialect: Box<dyn Dialect> = match effective_dialect {
        Some("postgres") => Box::new(PostgreSqlDialect {}),
        Some("sqlite") => Box::new(SQLiteDialect {}),
        Some("mysql") => Box::new(MySqlDialect {}),
        _ => Box::new(GenericDialect {}), // Why: permissive fallback when the dialect is unknown
    };

    // Dialect-aware placeholder validation using the resolved dialect's lexer so
    // dialect-specific quoting (e.g. MySQL `\'`) and comment syntax (e.g. MySQL `#`)
    // are respected.
    // Why: placeholder validation runs before parsing so a dialect-mismatched placeholder surfaces
    // as a placeholder error rather than a parse error.
    let tokens = Tokenizer::new(dialect.as_ref(), &content)
        .tokenize()
        .map_err(|e| format!("tokenizer error: {e}"))?;
    validate_placeholders(&tokens, effective_dialect)?;

    // Comment-aware scanning + syntax validation: `sqlparser-rs` tokenizes and parses the SQL.
    // Why: guard `Parser::parse_sql` (sqlparser 0.64.0) with `catch_unwind` so any unexpected
    // parser panic on malformed SQL turns into a diagnostic error rather than aborting `sql:check`.
    let parse_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Parser::parse_sql(dialect.as_ref(), &content)
    }));
    let statements = match parse_result {
        Ok(Ok(stmts)) => stmts,
        Ok(Err(e)) => {
            let msg = e.to_string();
            // Why: PostgreSqlDialect tokenizes bare `?` as `Token::Question` (the JSONB operator)
            // rather than `Token::Placeholder`. When `parse_sql` succeeds, `?` was a valid infix
            // JSONB operator; when it fails specifically at `found: ?`, it was a `?` placeholder.
            if effective_dialect == Some("postgres")
                && tokens.iter().any(|t| matches!(t, Token::Question))
                && msg.contains("found: ?")
            {
                return Err(
                    "PostgreSQL query uses '?' parameter placeholder instead of '$1, $2, ...'"
                        .to_string(),
                );
            }
            return Err(format!("SQL syntax error: {msg}"));
        }
        Err(payload) => {
            let detail = payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("internal parser panic on malformed SQL");
            return Err(format!("SQL syntax error: {detail}"));
        }
    };

    // A file that holds only comments (or nothing) yields no statements.
    // Why: the old code stripped comments by hand to detect emptiness; sqlparser gives us this
    // for free, comment-aware by construction.
    if statements.is_empty() {
        return Err("SQL file is empty (contains no query statements)".to_string());
    }

    Ok((statements, tokens, effective_dialect))
}

/// Enforce the per-dialect placeholder rule over already-tokenized SQL.
fn validate_placeholders(tokens: &[Token], dialect: Option<&str>) -> Result<(), String> {
    for token in tokens {
        match token {
            Token::Placeholder(value) => {
                let is_dollar_style = value.starts_with('$');
                match dialect {
                    Some("postgres") if !is_dollar_style => {
                        return Err(
                            "PostgreSQL query uses '?' parameter placeholder instead of '$1, $2, ...'"
                                .to_string(),
                        );
                    }
                    Some(dialect_name @ "sqlite") | Some(dialect_name @ "mysql")
                        if is_dollar_style =>
                    {
                        return Err(format!(
                            "{dialect_name} query uses PostgreSQL-style '$N' parameter placeholder instead of '?'"
                        ));
                    }
                    _ => {}
                }
            }
            // Why: MySqlDialect treats `$` as `is_identifier_start`, so `$1` tokenizes as an
            // unquoted `Token::Word` rather than `Token::Placeholder`. Reject unquoted `$N`
            // tokens in MySQL so PostgreSQL-style parameters are still caught.
            Token::Word(word)
                if dialect == Some("mysql")
                    && word.quote_style.is_none()
                    && word.value.starts_with('$')
                    && word.value.len() > 1
                    && word.value[1..].chars().all(|c| c.is_ascii_digit()) =>
            {
                return Err(
                    "mysql query uses PostgreSQL-style '$N' parameter placeholder instead of '?'"
                        .to_string(),
                );
            }
            _ => {}
        }
    }
    Ok(())
}

fn infer_dialect_from_path(path: &Path) -> Option<&'static str> {
    for comp in path.components() {
        let s = comp.as_os_str().to_string_lossy().to_ascii_lowercase();
        if s == "postgres" || s == "postgresql" || s == "pg" {
            return Some("postgres");
        } else if s == "sqlite" || s == "sqlite3" {
            return Some("sqlite");
        } else if s == "mysql" || s == "mariadb" {
            return Some("mysql");
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp(content: &str) -> tempfile::NamedTempFile {
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(temp.path(), content).unwrap();
        temp
    }

    #[test]
    fn sql_check_accepts_postgres_dollar_placeholder() {
        let temp = write_temp("SELECT * FROM users WHERE id = $1;");
        assert!(validate_sql_file(temp.path(), Some("postgres")).is_ok());
    }

    #[test]
    fn sql_check_accepts_sqlite_question_placeholder() {
        let temp = write_temp("SELECT * FROM users WHERE id = ?;");
        assert!(validate_sql_file(temp.path(), Some("sqlite")).is_ok());
    }

    #[test]
    fn sql_check_accepts_mysql_question_placeholder() {
        let temp = write_temp("SELECT * FROM users WHERE id = ?;");
        assert!(validate_sql_file(temp.path(), Some("mysql")).is_ok());
    }

    #[test]
    fn sql_check_rejects_postgres_question_placeholder() {
        let temp = write_temp("SELECT * FROM users WHERE id = ?;");
        let err = validate_sql_file(temp.path(), Some("postgres")).unwrap_err();
        assert!(
            err.contains("PostgreSQL query uses '?' parameter placeholder"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn sql_check_rejects_sqlite_dollar_placeholder() {
        let temp = write_temp("SELECT * FROM users WHERE id = $1;");
        let err = validate_sql_file(temp.path(), Some("sqlite")).unwrap_err();
        assert!(
            err.contains("sqlite query uses PostgreSQL-style '$N' parameter placeholder"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn sql_check_rejects_mysql_dollar_placeholder() {
        let temp = write_temp("SELECT * FROM users WHERE id = $1;");
        let err = validate_sql_file(temp.path(), Some("mysql")).unwrap_err();
        assert!(
            err.contains("mysql query uses PostgreSQL-style '$N' parameter placeholder"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn sql_check_ignores_placeholder_in_line_comment() {
        // `?` inside a `--` comment must not be flagged for postgres (which expects `$N`).
        let temp = write_temp("-- WHERE id = ?\nSELECT * FROM users;");
        assert!(validate_sql_file(temp.path(), Some("postgres")).is_ok());
    }

    #[test]
    fn sql_check_ignores_placeholder_in_hash_comment() {
        // `$1` inside a `#` comment must not be flagged for mysql (which supports `#` comments and
        // rejects `$1` placeholders).
        let temp = write_temp("# WHERE id = $1\nSELECT * FROM users;");
        assert!(validate_sql_file(temp.path(), Some("mysql")).is_ok());
    }

    #[test]
    fn sql_check_accepts_mysql_escaped_single_quote() {
        let temp = write_temp("SELECT 'it\\'s ready';");
        assert!(validate_sql_file(temp.path(), Some("mysql")).is_ok());
    }

    #[test]
    fn sql_check_accepts_postgres_jsonb_question_operators() {
        let q = write_temp("SELECT payload ? 'active' FROM events;");
        let (stmts, _tokens, _dialect) =
            validate_sql_file(q.path(), Some("postgres")).expect("jsonb ? parses");
        assert!(
            is_safe_explainable_dml(&stmts),
            "PostgreSQL JSONB `?` operator must remain eligible for live EXPLAIN"
        );

        let q_param = write_temp("SELECT payload ? 'active' FROM events WHERE id = $1;");
        let (stmts_p, _tokens_p, _dialect_p) =
            validate_sql_file(q_param.path(), Some("postgres")).expect("parameterized jsonb");
        assert!(
            !is_safe_explainable_dml(&stmts_p),
            "parameterized PostgreSQL query must be skipped from EXPLAIN"
        );

        let q_sqlite_named = write_temp("-- lookup\nSELECT :id;");
        let (stmts_named, _, _) =
            validate_sql_file(q_sqlite_named.path(), Some("sqlite")).expect("sqlite named param");
        assert!(
            !is_safe_explainable_dml(&stmts_named),
            "SQLite named placeholder `:id` must be excluded from EXPLAIN"
        );

        let q_mysql_match = write_temp("SELECT MATCH(body) AGAINST (?) FROM articles;");
        let (stmts_match, _, _) =
            validate_sql_file(q_mysql_match.path(), Some("mysql")).expect("mysql match against");
        assert!(
            !is_safe_explainable_dml(&stmts_match),
            "MySQL MATCH ... AGAINST (?) placeholder must be excluded from EXPLAIN"
        );

        let q_pipe = write_temp("SELECT payload ?| array['a', 'b'] FROM events;");
        assert!(validate_sql_file(q_pipe.path(), Some("postgres")).is_ok());

        let q_and = write_temp("SELECT payload ?& array['a', 'b'] FROM events;");
        assert!(validate_sql_file(q_and.path(), Some("postgres")).is_ok());
    }

    #[test]
    fn sql_check_rejects_malformed_mysql_div_without_panicking() {
        let malformed = write_temp("SELECT 1 DIV ;");
        assert!(validate_sql_file(malformed.path(), Some("mysql")).is_err());

        let valid = write_temp("SELECT 10 DIV 2;");
        assert!(validate_sql_file(valid.path(), Some("mysql")).is_ok());
    }

    #[test]
    fn sql_check_ignores_placeholder_in_block_comment() {
        // `$1` inside a `/* */` comment must not be flagged for sqlite (which rejects `$1`).
        let temp = write_temp("/* WHERE id = $1 */\nSELECT * FROM users;");
        assert!(validate_sql_file(temp.path(), Some("sqlite")).is_ok());
    }

    #[test]
    fn sql_check_rejects_unmatched_paren() {
        let temp = write_temp("SELECT * FROM users WHERE id IN (1, 2;");
        assert!(validate_sql_file(temp.path(), None).is_err());
    }

    #[test]
    fn sql_check_rejects_empty_sql() {
        let temp = write_temp("-- Just a comment\n");
        assert!(validate_sql_file(temp.path(), None).is_err());
    }

    #[test]
    fn sql_check_rejects_unrecognized_statement() {
        let temp = write_temp("FOOBAR something");
        assert!(validate_sql_file(temp.path(), None).is_err());
    }

    #[test]
    fn sql_check_errors_on_nonexistent_target_path() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does_not_exist.sql");
        let err = collect_target_sql_files(&missing)
            .expect_err("nonexistent target path must return an error");
        assert!(
            err.contains("does not exist"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn sql_check_accepts_single_sql_file_path() {
        let dir = tempfile::tempdir().unwrap();
        let single = dir.path().join("users.sql");
        fs::write(&single, "SELECT * FROM users WHERE id = $1;").unwrap();

        let files = collect_target_sql_files(&single)
            .expect("single .sql file path should be collected directly");
        assert_eq!(files, vec![single.clone()]);
        assert!(validate_sql_file(&files[0], Some("postgres")).is_ok());
    }

    #[test]
    fn sql_check_honors_db_dialect_when_cli_dialect_is_absent() {
        assert_eq!(
            resolve_target_dialect(None, Some("postgres")),
            Some("postgres")
        );
        assert_eq!(
            resolve_target_dialect(Some("mysql"), Some("postgres")),
            Some("mysql")
        );
        assert_eq!(resolve_target_dialect(None, None), None);

        // Verify that honoring the db dialect catches dialect-mismatched placeholders.
        let temp = write_temp("SELECT * FROM users WHERE id = ?;");
        let effective = resolve_target_dialect(None, Some("postgres"));
        assert!(
            validate_sql_file(temp.path(), effective).is_err(),
            "postgres db dialect should reject '?' placeholder when --dialect is omitted"
        );
    }

    #[cfg(unix)]
    #[test]
    fn collect_target_sql_files_skips_symlink_cycles() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("queries");
        fs::create_dir_all(&sub).unwrap();
        let sql_file = sub.join("list.sql");
        fs::write(&sql_file, "SELECT 1;").unwrap();
        std::os::unix::fs::symlink(dir.path(), sub.join("loop")).unwrap();

        let files = collect_target_sql_files(dir.path())
            .expect("collect_target_sql_files must not hang on symlink cycles");
        assert_eq!(files, vec![sql_file]);
    }
}
