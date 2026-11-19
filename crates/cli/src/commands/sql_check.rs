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
    let target_dir = Path::new(path_str);
    let mut files = Vec::new();
    if target_dir.exists() {
        collect_sql_files(target_dir, &mut files);
    }

    files.sort();

    let mut db_handle = None;
    if db_dry_run {
        match crate::commands::db::resolve_db_url() {
            Ok(Some(url)) => match crate::commands::db::connect_backend(&url).await {
                Ok((handle, dialect)) => {
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

    for file_path in &files {
        let display_path = file_path.display().to_string();
        let val_res = validate_sql_file(file_path, target_dialect);
        let final_res = match val_res {
            Ok((statements, tokens)) => {
                if let Some((handle, _dialect)) = &db_handle {
                    // Why: inspect the parsed `sqlparser` AST instead of raw string
                    // `first_word`. This both handles leading comments (`--` / `/* */`)
                    // and guarantees multi-statement SQL (e.g. `SELECT 1; DROP TABLE ...`)
                    // or parameterized queries are never sent to `EXPLAIN`.
                    if is_safe_explainable_dml(&statements, &tokens) {
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

fn collect_sql_files(dir: &Path, files: &mut Vec<PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_sql_files(&path, files);
            } else if path.extension().and_then(|s| s.to_str()) == Some("sql") {
                files.push(path);
            }
        }
    }
}

/// Returns `true` only when `statements` contains exactly one zero-parameter
/// `SELECT`/`INSERT`/`UPDATE`/`DELETE` statement safe to wrap in `EXPLAIN`.
fn is_safe_explainable_dml(statements: &[sqlparser::ast::Statement], tokens: &[Token]) -> bool {
    if statements.len() != 1 {
        return false;
    }
    if tokens
        .iter()
        .any(|t| matches!(t, Token::Placeholder(_) | Token::Question))
    {
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

/// Validate a single SQL file using `sqlparser-rs`.
///
/// Performs comment-aware syntax parsing, an emptiness check, and dialect-aware placeholder
/// validation. Comments (`--`, `#`, `/* */`) are tokenized as [`Token::Whitespace`] variants by
/// `sqlparser-rs`, so placeholders appearing inside comments are never flagged.
fn validate_sql_file(
    path: &Path,
    target_dialect: Option<&str>,
) -> Result<(Vec<sqlparser::ast::Statement>, Vec<Token>), String> {
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

    Ok((statements, tokens))
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
        assert!(validate_sql_file(q.path(), Some("postgres")).is_ok());

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
}
