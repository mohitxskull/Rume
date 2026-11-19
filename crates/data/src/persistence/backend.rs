//! Database backend and transaction abstractions.

use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;

use crate::persistence::error::PersistenceError;
use crate::persistence::row::DbRow;

/// Pinned boxed future type alias for async database traits.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Default audit log insert SQL for PostgreSQL backend.
const BACKEND_AUDIT_INSERT_POSTGRES: &str = include_str!("sql/postgres/audit_insert.sql");
/// Default audit log insert SQL for MySQL backend.
const BACKEND_AUDIT_INSERT_MYSQL: &str = include_str!("sql/mysql/audit_insert.sql");
/// Default audit log insert SQL for SQLite backend.
const BACKEND_AUDIT_INSERT_SQLITE: &str = include_str!("sql/sqlite/audit_insert.sql");

/// A portable SQL parameter value supported across database backends.
#[derive(Debug, Clone, PartialEq)]
pub enum DbValue {
    /// SQL NULL value.
    Null,
    /// UTF-8 string or text value.
    Text(String),
    /// 64-bit signed integer.
    Int(i64),
    /// 64-bit floating point number.
    Float(f64),
    /// Boolean flag.
    Bool(bool),
    /// Raw byte sequence / BLOB.
    Bytes(Vec<u8>),
}

impl From<&str> for DbValue {
    fn from(s: &str) -> Self {
        DbValue::Text(s.to_string())
    }
}

impl From<String> for DbValue {
    fn from(s: String) -> Self {
        DbValue::Text(s)
    }
}

impl From<&String> for DbValue {
    fn from(s: &String) -> Self {
        DbValue::Text(s.clone())
    }
}

impl From<i8> for DbValue {
    fn from(i: i8) -> Self {
        DbValue::Int(i as i64)
    }
}

impl From<i16> for DbValue {
    fn from(i: i16) -> Self {
        DbValue::Int(i as i64)
    }
}

impl From<i32> for DbValue {
    fn from(i: i32) -> Self {
        DbValue::Int(i as i64)
    }
}

impl From<i64> for DbValue {
    fn from(i: i64) -> Self {
        DbValue::Int(i)
    }
}

impl From<isize> for DbValue {
    fn from(i: isize) -> Self {
        DbValue::Int(i as i64)
    }
}

impl From<u8> for DbValue {
    fn from(u: u8) -> Self {
        DbValue::Int(u as i64)
    }
}

impl From<u16> for DbValue {
    fn from(u: u16) -> Self {
        DbValue::Int(u as i64)
    }
}

impl From<u32> for DbValue {
    fn from(u: u32) -> Self {
        DbValue::Int(u as i64)
    }
}

impl From<u64> for DbValue {
    fn from(u: u64) -> Self {
        if let Ok(i) = i64::try_from(u) {
            DbValue::Int(i)
        } else {
            DbValue::Text(u.to_string())
        }
    }
}

impl From<usize> for DbValue {
    fn from(u: usize) -> Self {
        // Why: Delegate through `u64` rather than casting `u as i64` directly so `usize`
        // values above `i64::MAX` degrade losslessly to `DbValue::Text` instead of wrapping to
        // negative `DbValue::Int` without altering the `DbValue::Int(i64)` wire variant
        // (revisit only if `DbValue` adds an unsigned integer variant).
        Self::from(u as u64)
    }
}

impl From<f32> for DbValue {
    fn from(f: f32) -> Self {
        DbValue::Float(f as f64)
    }
}

impl From<f64> for DbValue {
    fn from(f: f64) -> Self {
        DbValue::Float(f)
    }
}

impl From<bool> for DbValue {
    fn from(b: bool) -> Self {
        DbValue::Bool(b)
    }
}

impl From<Vec<u8>> for DbValue {
    fn from(bytes: Vec<u8>) -> Self {
        DbValue::Bytes(bytes)
    }
}

impl From<&[u8]> for DbValue {
    fn from(bytes: &[u8]) -> Self {
        DbValue::Bytes(bytes.to_vec())
    }
}

impl<T: Into<DbValue>> From<Option<T>> for DbValue {
    fn from(opt: Option<T>) -> Self {
        match opt {
            Some(v) => v.into(),
            None => DbValue::Null,
        }
    }
}

impl<T: Clone + Into<DbValue>> From<&Option<T>> for DbValue {
    fn from(opt: &Option<T>) -> Self {
        match opt {
            Some(v) => v.clone().into(),
            None => DbValue::Null,
        }
    }
}

/// Convenience macro for constructing an array of [`DbValue`] parameters suitable for borrowing as a slice.
#[macro_export]
macro_rules! params {
    ($($val:expr),* $(,)?) => {
        [
            $(
                $crate::persistence::backend::DbValue::from($val)
            ),*
        ]
    };
}

/// Generic interface implemented by database drivers and storage backends.
pub trait DatabaseBackend: Debug + Send + Sync + 'static {
    /// Type of transaction produced by this backend.
    type Transaction: DatabaseTransaction + Clone + 'static;

    /// Verifies connectivity to the database.
    fn ping(&self) -> BoxFuture<'_, Result<(), PersistenceError>>;

    /// Executes a SQL statement directly with bound parameters, returning the number of affected rows.
    fn execute_params(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<u64, PersistenceError>>;

    /// Executes a SQL or DDL statement directly, returning the number of affected rows.
    fn execute(&self, statement: &str) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        self.execute_params(statement, &[])
    }

    /// Executes a scalar query with bound parameters returning an optional string value.
    fn query_scalar_params(
        &self,
        query: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Option<String>, PersistenceError>>;

    /// Executes a scalar query returning an optional string value.
    fn query_scalar(&self, query: &str) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
        self.query_scalar_params(query, &[])
    }

    /// Fetches all rows matching the parameterized query as generic [`DbRow`] records.
    fn fetch_all(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Vec<DbRow>, PersistenceError>>;

    /// Fetches an optional single row matching the parameterized query as a [`DbRow`].
    fn fetch_optional(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Option<DbRow>, PersistenceError>>;

    /// Fetches exactly one row matching the parameterized query, returning an error if zero rows match.
    fn fetch_one(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<DbRow, PersistenceError>> {
        let stmt = statement.to_string();
        let p = params.to_vec();
        Box::pin(async move {
            self.fetch_optional(&stmt, &p).await?.ok_or_else(|| {
                PersistenceError::QueryFailed("query returned zero rows, expected one".into())
            })
        })
    }

    /// Begins a new transaction.
    fn begin_transaction(&self) -> BoxFuture<'_, Result<Self::Transaction, PersistenceError>>;

    /// Returns the current number of open physical connections managed by the backend driver.
    fn pool_size(&self) -> u32 {
        0
    }

    /// Returns the number of idle connections currently available in the backend pool.
    fn pool_idle(&self) -> u32 {
        0
    }

    /// Returns whether the underlying driver pool has been closed.
    fn is_closed(&self) -> bool {
        false
    }

    /// Closes the physical database connection pool.
    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {})
    }

    /// Returns the database dialect (e.g. "postgres", "mysql", "sqlite", "default").
    fn dialect(&self) -> &'static str {
        "default"
    }

    /// Returns the underlying MySQL pool when this backend (or a wrapper around it)
    /// supports session-pinned `GET_LOCK`/`RELEASE_LOCK` migration locking.
    ///
    /// Wrappers around [`crate::persistence::MysqlBackend`] MUST forward this method so
    /// [`crate::persistence::Migrator`] acquires the database-wide migration lock before
    /// reading or mutating `rume_migrations`.
    #[cfg(feature = "mysql")]
    fn mysql_migration_pool(&self) -> Option<&sqlx::MySqlPool> {
        None
    }

    /// Records an audit log event using parameterized query binding.
    #[allow(clippy::too_many_arguments)]
    fn record_audit_log(
        &self,
        id: &str,
        action: &str,
        resource: &str,
        status: &str,
        actor_id: Option<&str>,
        target_id: Option<&str>,
        ip_address: Option<&str>,
        metadata: &str,
        created_at: i64,
    ) -> BoxFuture<'_, Result<(), PersistenceError>> {
        let is_postgres = self.dialect() == "postgres";
        let stmt = if is_postgres {
            BACKEND_AUDIT_INSERT_POSTGRES
        } else if self.dialect() == "mysql" {
            BACKEND_AUDIT_INSERT_MYSQL
        } else {
            BACKEND_AUDIT_INSERT_SQLITE
        };
        let params = vec![
            DbValue::from(id),
            DbValue::from(action),
            DbValue::from(resource),
            DbValue::from(status),
            DbValue::from(actor_id),
            DbValue::from(target_id),
            DbValue::from(ip_address),
            DbValue::from(metadata),
            DbValue::from(created_at),
        ];
        Box::pin(async move {
            self.execute_params(stmt, &params).await?;
            Ok(())
        })
    }
}

#[cfg(any(feature = "postgres", feature = "mysql", feature = "sqlite"))]
pub(crate) fn map_sqlx_error(err: sqlx::Error) -> PersistenceError {
    match err {
        sqlx::Error::PoolTimedOut => PersistenceError::Timeout,
        sqlx::Error::PoolClosed => PersistenceError::Closed,
        sqlx::Error::Io(e) => PersistenceError::ConnectionFailed(e.to_string()),
        other => PersistenceError::QueryFailed(other.to_string()),
    }
}

#[cfg(any(feature = "postgres", feature = "mysql", feature = "sqlite", test))]
fn check_managed_tx_control_keywords(words: &[String]) -> Result<(), PersistenceError> {
    let Some(first) = words.first().map(String::as_str) else {
        return Ok(());
    };
    let second = words.get(1).map(String::as_str);
    let third = words.get(2).map(String::as_str);

    let is_tx_control = match first {
        "COMMIT" | "END" | "ABORT" | "BEGIN" => true,
        "START" | "PREPARE" => second == Some("TRANSACTION"),
        "ROLLBACK" => {
            let is_savepoint = second == Some("TO")
                || (matches!(second, Some("WORK" | "TRANSACTION")) && third == Some("TO"));
            !is_savepoint
        }
        _ => false,
    };

    if is_tx_control {
        return Err(PersistenceError::TransactionError(format!(
            "transaction-control statement '{first}' is not allowed inside a managed transaction"
        )));
    }

    Ok(())
}

/// Normalizes C-style non-nested block comments (`/* ... /* ... */`) for dialects that close
/// block comments at the first `*/` (SQLite and MySQL), while leaving PostgreSQL's nested
/// `/* ... /* ... */ ... */` comments untouched.
///
/// Why: `sqlparser = "=0.53.0"` unconditionally counts every inner `/*` inside a block comment
/// as a nested opener, causing valid SQLite/MySQL SQL like `SELECT 1 /* example /* marker */`
/// to fail with `"Unexpected EOF while in a multi-line comment"`. Replacing inner `/*` with
/// `/ ` (same byte length, preserving token spans) inside a comment up to the first `*/` aligns
/// `sqlparser` with SQLite's `tokenize.c` without weakening unclosed-comment rejection.
#[cfg(any(feature = "postgres", feature = "mysql", feature = "sqlite", test))]
pub(crate) fn sanitize_non_nested_block_comments<'a>(
    statement: &'a str,
    dialect: &dyn sqlparser::dialect::Dialect,
) -> std::borrow::Cow<'a, str> {
    if dialect.is::<sqlparser::dialect::PostgreSqlDialect>() || !statement.contains("/*") {
        return std::borrow::Cow::Borrowed(statement);
    }

    let is_mysql = dialect.is::<sqlparser::dialect::MySqlDialect>();
    let is_sqlite = dialect.is::<sqlparser::dialect::SQLiteDialect>();
    let bytes = statement.as_bytes();
    let mut out: Option<Vec<u8>> = None;
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'\'' | b'"' | b'`' => {
                let quote = b;
                i += 1;
                while i < bytes.len() {
                    if is_mysql && (quote == b'\'' || quote == b'"') && bytes[i] == b'\\' {
                        i = (i + 2).min(bytes.len());
                        continue;
                    }
                    if bytes[i] == quote {
                        i += 1;
                        if i < bytes.len() && bytes[i] == quote {
                            i += 1;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
            }
            b'[' if is_sqlite => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b']' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'#' if is_mysql => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() {
                    if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        i += 2;
                        break;
                    }
                    if bytes[i] == b'/' && bytes[i + 1] == b'*' {
                        let buf = out.get_or_insert_with(|| bytes.to_vec());
                        buf[i + 1] = b' ';
                        i += 2;
                        continue;
                    }
                    i += 1;
                }
            }
            _ => {
                i += 1;
            }
        }
    }

    match out {
        Some(vec) => std::borrow::Cow::Owned(
            String::from_utf8(vec).expect("replacing ASCII '*' with ' ' preserves valid UTF-8"),
        ),
        None => std::borrow::Cow::Borrowed(statement),
    }
}

/// Tokenizes `statement` with `sqlparser` and strips whitespace tokens. When `is_mysql` is
/// `true`, MySQL executable block comments (`/*! ... */` and `/*!50000 ... */`) are rejected
/// outright so neither comment-smuggled transaction control (`/*! COMMIT */`) nor version-gated
/// DDL with an unresolvable server-version predicate (`/*!99999 CREATE TABLE ... */`) can
/// misclassify or commit a managed transaction/migration.
#[cfg(any(feature = "postgres", feature = "mysql", feature = "sqlite", test))]
fn tokenize_sql_filtered(
    statement: &str,
    dialect: &dyn sqlparser::dialect::Dialect,
    is_mysql: bool,
) -> Result<Vec<sqlparser::tokenizer::Token>, PersistenceError> {
    use sqlparser::tokenizer::{Token, Tokenizer, Whitespace};

    let sanitized = sanitize_non_nested_block_comments(statement, dialect);
    let raw_tokens = Tokenizer::new(dialect, &sanitized)
        .tokenize()
        .map_err(|e| {
            PersistenceError::TransactionError(format!("failed to tokenize SQL statement: {e}"))
        })?;

    let mut out = Vec::with_capacity(raw_tokens.len());
    for tok in raw_tokens {
        match tok {
            Token::Whitespace(Whitespace::MultiLineComment(ref content))
                if is_mysql && content.starts_with('!') =>
            {
                return Err(PersistenceError::TransactionError(
                    "MySQL executable comments (`/*! ... */`) are not allowed in managed transactions or migrations"
                        .into(),
                ));
            }
            Token::Whitespace(_) => {}
            other => out.push(other),
        }
    }
    Ok(out)
}

/// Scans pre-tokenized `tokens` into top-level statement leading-word triples (up to 3 uppercased
/// unquoted words per statement), keeping semicolons inside compound SQL bodies
/// (`CREATE TRIGGER ... BEGIN ...; END;` in SQLite and `CREATE FUNCTION/PROCEDURE ...
/// BEGIN ATOMIC ...; END;` in PostgreSQL) attached to the enclosing `CREATE` statement
/// rather than splitting them into bogus top-level `END` statements.
#[cfg(any(feature = "postgres", feature = "mysql", feature = "sqlite", test))]
fn collect_top_level_statement_heads_from_tokens(
    tokens: &[sqlparser::tokenizer::Token],
) -> Vec<Vec<String>> {
    use sqlparser::tokenizer::Token;

    let mut heads: Vec<Vec<String>> = Vec::new();
    let mut leading_words: Vec<String> = Vec::with_capacity(3);
    let mut block_depth: usize = 0;

    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i] {
            Token::SemiColon => {
                if block_depth == 0 && !leading_words.is_empty() {
                    heads.push(std::mem::take(&mut leading_words));
                }
            }
            Token::Word(word) if word.quote_style.is_none() => {
                let upper = word.value.to_ascii_uppercase();
                if block_depth == 0 {
                    if leading_words.len() < 3 {
                        leading_words.push(upper.clone());
                    }
                    // Enter a compound body only when `BEGIN` appears inside an already-started
                    // `CREATE` statement (e.g. SQLite `CREATE TRIGGER ... BEGIN ... END;` or
                    // PostgreSQL `CREATE FUNCTION ... BEGIN ATOMIC ... END;`). A standalone
                    // `BEGIN` at statement start stays at `block_depth == 0` so it is caught
                    // as transaction control.
                    if upper == "BEGIN"
                        && leading_words.first().map(String::as_str) == Some("CREATE")
                    {
                        block_depth = 1;
                    }
                } else {
                    match upper.as_str() {
                        "BEGIN" | "CASE" => {
                            block_depth += 1;
                        }
                        "END" => {
                            // Do not decrement for `END IF` / `END LOOP` / `END WHILE` / `END REPEAT`
                            // because we only increment for `BEGIN` and `CASE`.
                            let next_is_non_block_closer = matches!(
                                tokens.get(i + 1),
                                Some(Token::Word(next))
                                    if next.quote_style.is_none()
                                        && matches!(
                                            next.value.to_ascii_uppercase().as_str(),
                                            "IF" | "LOOP" | "WHILE" | "REPEAT"
                                        )
                            );
                            if !next_is_non_block_closer {
                                block_depth = block_depth.saturating_sub(1);
                                // Consume optional `END CASE` pair without double-processing.
                                if matches!(
                                    tokens.get(i + 1),
                                    Some(Token::Word(next))
                                        if next.quote_style.is_none()
                                            && next.value.eq_ignore_ascii_case("CASE")
                                ) {
                                    i += 1;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }

    if !leading_words.is_empty() {
        heads.push(leading_words);
    }

    heads
}

/// Inspects a parsed `sqlparser::ast::Statement` for explicit transaction control.
#[cfg(any(feature = "postgres", feature = "mysql", feature = "sqlite", test))]
fn check_ast_tx_control(stmt: &sqlparser::ast::Statement) -> Result<(), PersistenceError> {
    use sqlparser::ast::Statement;

    let offending = match stmt {
        Statement::Commit { .. } => Some("COMMIT"),
        Statement::StartTransaction { .. } => Some("START TRANSACTION"),
        Statement::Rollback { savepoint, .. } if savepoint.is_none() => Some("ROLLBACK"),
        _ => None,
    };
    if let Some(kw) = offending {
        return Err(PersistenceError::TransactionError(format!(
            "transaction-control statement '{kw}' is not allowed inside a managed transaction"
        )));
    }
    Ok(())
}

/// Inspects a parsed `sqlparser::ast::Statement` for MySQL implicit-commit DDL.
#[cfg(any(feature = "mysql", test))]
fn is_mysql_implicit_commit_ast(stmt: &sqlparser::ast::Statement) -> bool {
    use sqlparser::ast::Statement;

    match stmt {
        Statement::CreateTable(ct) => !ct.temporary,
        Statement::Drop { temporary, .. } => !*temporary,
        Statement::CreateIndex(_)
        | Statement::CreateView { .. }
        | Statement::CreateRole { .. }
        | Statement::CreateSchema { .. }
        | Statement::CreateDatabase { .. }
        | Statement::CreateFunction(_)
        | Statement::CreateProcedure { .. }
        | Statement::CreateTrigger { .. }
        | Statement::CreateSequence { .. }
        | Statement::CreateType { .. }
        | Statement::CreateVirtualTable { .. }
        | Statement::DropFunction { .. }
        | Statement::DropProcedure { .. }
        | Statement::DropTrigger { .. }
        | Statement::AlterTable { .. }
        | Statement::AlterIndex { .. }
        | Statement::AlterView { .. }
        | Statement::AlterRole { .. }
        | Statement::Truncate { .. }
        | Statement::LockTables { .. }
        | Statement::UnlockTables
        | Statement::Grant { .. }
        | Statement::Revoke { .. }
        | Statement::Analyze { .. }
        | Statement::Cache { .. }
        | Statement::UNCache { .. }
        | Statement::Flush { .. }
        | Statement::OptimizeTable { .. }
        | Statement::Install { .. }
        | Statement::Load { .. }
        | Statement::LoadData { .. } => true,
        _ => false,
    }
}

/// Rejects top-level transaction-control statements in a raw SQL batch so a managed
/// [`DatabaseTransaction`] cannot be committed or rolled back out-of-band.
///
/// Why: Delegate SQL lexing and AST inspection to `sqlparser = "=0.53.0"` instead of maintaining
/// hand-rolled dialect scanners; fail closed on `TokenizerError` so malformed quotes cannot
/// smuggle transaction-control keywords past the guard.
// Why: MySQL transactions call `validate_managed_mysql_transaction_sql` instead (it adds the
// implicit-commit rules), so compiling this generic validator under a `mysql`-only build is
// dead code that `-D warnings` rejects. Revisit if MySQL starts delegating to it.
#[cfg(any(feature = "postgres", feature = "sqlite", test))]
pub(crate) fn validate_managed_transaction_sql(
    statement: &str,
    dialect: &dyn sqlparser::dialect::Dialect,
) -> Result<(), PersistenceError> {
    let is_mysql = dialect.is::<sqlparser::dialect::MySqlDialect>();
    let tokens = tokenize_sql_filtered(statement, dialect, is_mysql)?;
    if !tokens.is_empty()
        && let Ok(stmts) = sqlparser::parser::Parser::new(dialect)
            .with_tokens(tokens.clone())
            .parse_statements()
    {
        for stmt in &stmts {
            check_ast_tx_control(stmt)?;
        }
    }
    for words in collect_top_level_statement_heads_from_tokens(&tokens) {
        check_managed_tx_control_keywords(&words)?;
    }
    Ok(())
}

/// Returns `true` if a top-level statement head causes an implicit commit in MySQL
/// (MySQL 8.4 Ref Manual §15.3.3). `CREATE TEMPORARY TABLE` and `DROP TEMPORARY TABLE`
/// do NOT cause an implicit commit and are allowed.
#[cfg(any(feature = "mysql", test))]
fn is_mysql_implicit_commit_head(words: &[String]) -> bool {
    let Some(first) = words.first().map(String::as_str) else {
        return false;
    };
    let second = words.get(1).map(String::as_str);
    match first {
        "CREATE" | "DROP" => second != Some("TEMPORARY"),
        "ALTER" | "TRUNCATE" | "RENAME" | "LOCK" | "UNLOCK" | "LOAD" | "GRANT" | "REVOKE"
        | "ANALYZE" | "CACHE" | "CHECK" | "FLUSH" | "OPTIMIZE" | "REPAIR" | "RESET" | "INSTALL"
        | "UNINSTALL" | "XA" => true,
        "SET" => second == Some("PASSWORD"),
        _ => false,
    }
}

/// Statement-level keyword scope inherited by subsequent bare assignments in a MySQL `SET`
/// statement (`SET GLOBAL a = 1, b = 2` vs `SET SESSION a = 1, b = 2`).
#[cfg(any(feature = "mysql", test))]
#[derive(Clone, Copy, PartialEq, Eq)]
enum InheritedSetScope {
    /// Default (`SESSION` / `LOCAL`): a bare `autocommit` assignment targets the current session.
    Session,
    /// `GLOBAL` / `PERSIST` / `PERSIST_ONLY`: a bare `autocommit` assignment targets non-session scope.
    NonSession,
}

/// Evaluates `lhs` tokens (the left-hand side of `=` or `:=` in a MySQL `SET` assignment),
/// updating `inherited_scope` when a statement-level scope keyword (`GLOBAL`, `PERSIST`,
/// `PERSIST_ONLY`, `SESSION`, `LOCAL`) is present and returning `true` if this assignment
/// targets the current session's `autocommit` system variable.
///
/// Why: In MySQL 8.4 (`SET Syntax for Variable Assignment`), a keyword scope (`GLOBAL`,
/// `SESSION`, etc.) applies to subsequent bare variable names in the same comma-separated `SET`
/// statement (`SET GLOBAL sort_buffer_size = 1000000, autocommit = 1` sets global `autocommit`),
/// whereas an `@@` qualifier (`@@global.sort_buffer_size = 1000000, autocommit = 1`) applies
/// only to its own assignment target. User variables (`@autocommit`, `@saved_autocommit`),
/// non-session scopes, and RHS reads never mutate the current session's `autocommit`.
#[cfg(any(feature = "mysql", test))]
fn evaluate_mysql_set_lhs(
    lhs: &[sqlparser::tokenizer::Token],
    inherited_scope: &mut InheritedSetScope,
) -> bool {
    use sqlparser::tokenizer::Token;

    let is_autocommit_word = |tok: &Token| -> bool {
        matches!(tok, Token::Word(w) if w.value.eq_ignore_ascii_case("autocommit"))
    };
    let is_explicit_session_qualifier = |tok: &Token| -> bool {
        matches!(
            tok,
            Token::Word(w)
                if w.quote_style.is_none()
                    && matches!(
                        w.value.to_ascii_lowercase().as_str(),
                        "@@session" | "@@local" | "@@"
                    )
        )
    };

    match lhs {
        [Token::Word(w)] => {
            if w.quote_style.is_none() && w.value.eq_ignore_ascii_case("@@autocommit") {
                true
            } else if w.value.eq_ignore_ascii_case("autocommit") {
                *inherited_scope == InheritedSetScope::Session
            } else {
                false
            }
        }
        [Token::Word(scope), target] if scope.quote_style.is_none() => {
            match scope.value.to_ascii_lowercase().as_str() {
                "session" | "local" => {
                    *inherited_scope = InheritedSetScope::Session;
                    is_autocommit_word(target)
                }
                "global" | "persist" | "persist_only" => {
                    *inherited_scope = InheritedSetScope::NonSession;
                    false
                }
                "@@" => is_autocommit_word(target),
                _ => false,
            }
        }
        [scope, Token::Period, target] => {
            is_explicit_session_qualifier(scope) && is_autocommit_word(target)
        }
        [Token::Word(at), Token::Word(scope), Token::Period, target]
            if at.quote_style.is_none() && at.value == "@@" && scope.quote_style.is_none() =>
        {
            matches!(
                scope.value.to_ascii_lowercase().as_str(),
                "session" | "local"
            ) && is_autocommit_word(target)
        }
        _ => false,
    }
}

/// Rejects MySQL `SET` assignments whose LHS target is the session `autocommit` system variable.
#[cfg(any(feature = "mysql", test))]
fn check_mysql_autocommit_assignment_tokens(
    tokens: &[sqlparser::tokenizer::Token],
) -> Result<(), PersistenceError> {
    use sqlparser::tokenizer::Token;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum SetScanState {
        OutsideSet,
        InSetLhs,
        InSetRhs { paren_depth: usize },
    }

    let mut state = SetScanState::OutsideSet;
    let mut inherited_scope = InheritedSetScope::Session;
    let mut at_statement_start = true;
    let mut lhs_tokens: Vec<Token> = Vec::new();

    for token in tokens {
        match token {
            Token::SemiColon => {
                state = SetScanState::OutsideSet;
                inherited_scope = InheritedSetScope::Session;
                at_statement_start = true;
                lhs_tokens.clear();
            }
            Token::Word(w) if at_statement_start => {
                at_statement_start = false;
                if w.quote_style.is_none() && w.value.eq_ignore_ascii_case("SET") {
                    state = SetScanState::InSetLhs;
                    inherited_scope = InheritedSetScope::Session;
                    lhs_tokens.clear();
                }
            }
            _ => {
                at_statement_start = false;
                match &mut state {
                    SetScanState::OutsideSet => {}
                    SetScanState::InSetLhs => match token {
                        Token::Eq | Token::Assignment => {
                            if evaluate_mysql_set_lhs(&lhs_tokens, &mut inherited_scope) {
                                return Err(PersistenceError::TransactionError(
                                    "assigning session `autocommit` is not allowed inside a managed transaction (it can commit pending writes)"
                                        .into(),
                                ));
                            }
                            lhs_tokens.clear();
                            state = SetScanState::InSetRhs { paren_depth: 0 };
                        }
                        other => lhs_tokens.push(other.clone()),
                    },
                    SetScanState::InSetRhs { paren_depth } => match token {
                        Token::LParen => *paren_depth += 1,
                        Token::RParen => *paren_depth = paren_depth.saturating_sub(1),
                        Token::Comma if *paren_depth == 0 => {
                            lhs_tokens.clear();
                            state = SetScanState::InSetLhs;
                        }
                        _ => {}
                    },
                }
            }
        }
    }
    Ok(())
}

/// Returns `true` if `statement` contains any MySQL implicit-commit statement (e.g. DDL).
///
/// Why: errors on explicit transaction control (`COMMIT`, `ROLLBACK`, `START TRANSACTION`, ...),
/// session `autocommit` changes, and `/*! ... */` executable comments instead of classifying them.
#[cfg(any(feature = "mysql", test))]
pub(crate) fn mysql_sql_causes_implicit_commit(statement: &str) -> Result<bool, PersistenceError> {
    let dialect = sqlparser::dialect::MySqlDialect {};
    let tokens = tokenize_sql_filtered(statement, &dialect, true)?;
    check_mysql_autocommit_assignment_tokens(&tokens)?;

    let mut ast_implicit_commit = false;
    if !tokens.is_empty()
        && let Ok(stmts) = sqlparser::parser::Parser::new(&dialect)
            .with_tokens(tokens.clone())
            .parse_statements()
    {
        for stmt in &stmts {
            check_ast_tx_control(stmt)?;
            if is_mysql_implicit_commit_ast(stmt) {
                ast_implicit_commit = true;
            }
        }
    }

    let heads = collect_top_level_statement_heads_from_tokens(&tokens);
    for words in &heads {
        check_managed_tx_control_keywords(words)?;
    }
    Ok(ast_implicit_commit || heads.iter().any(|w| is_mysql_implicit_commit_head(w)))
}

/// Validates raw SQL executed inside a managed MySQL transaction, rejecting explicit
/// transaction-control statements, MySQL implicit-commit statements (`CREATE TABLE`,
/// `ALTER TABLE`, `DROP TABLE`, session `autocommit` assignments), and `/*! ... */` executable
/// comments that could silently commit pending InnoDB updates on the server.
#[cfg(any(feature = "mysql", test))]
pub(crate) fn validate_managed_mysql_transaction_sql(
    statement: &str,
) -> Result<(), PersistenceError> {
    let dialect = sqlparser::dialect::MySqlDialect {};
    let tokens = tokenize_sql_filtered(statement, &dialect, true)?;
    check_mysql_autocommit_assignment_tokens(&tokens)?;

    if !tokens.is_empty()
        && let Ok(stmts) = sqlparser::parser::Parser::new(&dialect)
            .with_tokens(tokens.clone())
            .parse_statements()
    {
        for stmt in &stmts {
            check_ast_tx_control(stmt)?;
            if is_mysql_implicit_commit_ast(stmt) {
                return Err(PersistenceError::TransactionError(
                    "MySQL implicit-commit DDL statement is not allowed inside a managed transaction; execute DDL on MysqlBackend directly"
                        .into(),
                ));
            }
        }
    }

    let heads = collect_top_level_statement_heads_from_tokens(&tokens);
    for words in &heads {
        check_managed_tx_control_keywords(words)?;
        if is_mysql_implicit_commit_head(words) {
            let first = words.first().map(String::as_str).unwrap_or("DDL");
            return Err(PersistenceError::TransactionError(format!(
                "MySQL implicit-commit statement '{first}' is not allowed inside a managed transaction; execute DDL on MysqlBackend directly"
            )));
        }
    }
    Ok(())
}

/// Active transaction handle providing isolated mutation and explicit commit/rollback.
pub trait DatabaseTransaction: Debug + Send + Sync {
    /// Returns the database dialect of this transaction (e.g. "postgres", "sqlite", "mysql", "default").
    fn dialect(&self) -> &'static str {
        "default"
    }

    /// Executes a parameterized statement within the transaction boundary.
    fn execute_params(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<u64, PersistenceError>>;

    /// Executes a statement within the transaction boundary.
    fn execute(&self, statement: &str) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        self.execute_params(statement, &[])
    }

    /// Queries a scalar value within the transaction boundary with bound parameters.
    fn query_scalar_params(
        &self,
        query: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Option<String>, PersistenceError>>;

    /// Queries a scalar value within the transaction boundary.
    fn query_scalar(&self, query: &str) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
        self.query_scalar_params(query, &[])
    }

    /// Fetches all rows matching the parameterized query within the transaction boundary.
    fn fetch_all(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Vec<DbRow>, PersistenceError>>;

    /// Fetches an optional single row matching the parameterized query within the transaction boundary.
    fn fetch_optional(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Option<DbRow>, PersistenceError>>;

    /// Fetches exactly one row matching the parameterized query within the transaction boundary.
    fn fetch_one(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<DbRow, PersistenceError>> {
        let stmt = statement.to_string();
        let p = params.to_vec();
        Box::pin(async move {
            self.fetch_optional(&stmt, &p).await?.ok_or_else(|| {
                PersistenceError::QueryFailed("query returned zero rows, expected one".into())
            })
        })
    }

    /// Atomically commits all changes made within this transaction.
    fn commit(&self) -> BoxFuture<'_, Result<(), PersistenceError>>;

    /// Explicitly discards and rolls back all changes made within this transaction.
    fn rollback(&self) -> BoxFuture<'_, Result<(), PersistenceError>>;

    /// Marks the transaction connection for cancellation-safe session cleanup if dropped
    /// without explicit resolution (e.g. releasing a MySQL session-level named lock).
    fn release_mysql_lock_on_drop(&self, _armed: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_value_from_usize_handles_values_above_i64_max() {
        assert_eq!(DbValue::from(42usize), DbValue::Int(42));
        #[cfg(target_pointer_width = "64")]
        assert_eq!(
            DbValue::from(usize::MAX),
            DbValue::Text(usize::MAX.to_string())
        );
    }

    #[test]
    fn rejects_transaction_control_statements_in_raw_batches() {
        let pg = sqlparser::dialect::PostgreSqlDialect {};
        for sql in [
            "COMMIT",
            "  commit ; ",
            "INSERT INTO items (id) VALUES (1); COMMIT;",
            "ROLLBACK",
            "ROLLBACK TRANSACTION;",
            "BEGIN; INSERT INTO items (id) VALUES (1);",
            "START TRANSACTION;",
            "END;",
            "ABORT;",
            "PREPARE TRANSACTION 'tx_1';",
        ] {
            let err = validate_managed_transaction_sql(sql, &pg)
                .expect_err(&format!("expected rejection for: {sql}"));
            assert!(
                matches!(err, PersistenceError::TransactionError(_)),
                "expected TransactionError for {sql}, got {err:?}"
            );
        }
    }

    #[test]
    fn allows_non_resolving_batches_and_quoted_or_commented_keywords() {
        let pg = sqlparser::dialect::PostgreSqlDialect {};
        for sql in [
            "INSERT INTO items (id) VALUES (1); UPDATE items SET id = 2;",
            "SELECT 'COMMIT', E'it\\'s ROLLBACK';",
            "/* COMMIT */ SELECT 1;",
            "/* outer /* ROLLBACK */ inner */ SELECT 1;",
            "-- BEGIN\nINSERT INTO items (id) VALUES (1);",
            "DO $$ BEGIN INSERT INTO items (id) VALUES (1); END $$;",
            "SAVEPOINT sp1; ROLLBACK TO SAVEPOINT sp1; RELEASE SAVEPOINT sp1;",
            "ROLLBACK WORK TO sp1;",
        ] {
            validate_managed_transaction_sql(sql, &pg)
                .unwrap_or_else(|e| panic!("expected '{sql}' to be allowed, got: {e:?}"));
        }
    }

    #[test]
    fn validate_managed_transaction_sql_handles_all_dialects_comments_and_quotes() {
        let pg = sqlparser::dialect::PostgreSqlDialect {};
        let mysql = sqlparser::dialect::MySqlDialect {};
        let sqlite = sqlparser::dialect::SQLiteDialect {};

        validate_managed_transaction_sql("SELECT `COMMIT` FROM t;", &mysql)
            .expect("MySQL backtick-quoted COMMIT identifier must pass");
        validate_managed_transaction_sql("# COMMIT\nSELECT 1;", &mysql)
            .expect("MySQL # single-line comment must pass");
        validate_managed_transaction_sql("SELECT [BEGIN] FROM t;", &sqlite)
            .expect("SQLite bracket-quoted BEGIN identifier must pass");
        validate_managed_transaction_sql("DO $$ BEGIN INSERT INTO t VALUES (1); END $$;", &pg)
            .expect("PostgreSQL dollar-quoted block must pass");

        let dialects: [(&str, &dyn sqlparser::dialect::Dialect); 3] =
            [("postgres", &pg), ("mysql", &mysql), ("sqlite", &sqlite)];
        for (name, dialect) in dialects {
            for sql in ["COMMIT", "ROLLBACK", "BEGIN", "START TRANSACTION"] {
                let err = validate_managed_transaction_sql(sql, dialect)
                    .expect_err(&format!("{name} must reject raw '{sql}'"));
                assert!(
                    matches!(err, PersistenceError::TransactionError(_)),
                    "{name} expected TransactionError for '{sql}', got: {err:?}"
                );
            }
        }

        #[cfg(feature = "sqlite")]
        {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build current-thread runtime");
            rt.block_on(async {
                let backend = crate::persistence::SqliteBackend::in_memory()
                    .await
                    .expect("in-memory sqlite");
                let tx = backend.begin_transaction().await.expect("begin sqlite tx");
                for sql in ["COMMIT", "BEGIN", "ROLLBACK"] {
                    let err = tx
                        .execute_params(sql, &[])
                        .await
                        .expect_err(&format!("SqliteTransaction must reject '{sql}'"));
                    assert!(
                        matches!(err, PersistenceError::TransactionError(_)),
                        "expected TransactionError for '{sql}', got: {err:?}"
                    );
                }
            });
        }
    }

    #[cfg(any(feature = "postgres", feature = "mysql", feature = "sqlite"))]
    #[test]
    fn map_sqlx_error_preserves_driver_classifications() {
        assert!(matches!(
            map_sqlx_error(sqlx::Error::PoolTimedOut),
            PersistenceError::Timeout
        ));
        assert!(matches!(
            map_sqlx_error(sqlx::Error::PoolClosed),
            PersistenceError::Closed
        ));
        let io_err = sqlx::Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "peer reset",
        ));
        assert!(matches!(
            map_sqlx_error(io_err),
            PersistenceError::ConnectionFailed(msg) if msg.contains("peer reset")
        ));
        assert!(matches!(
            map_sqlx_error(sqlx::Error::RowNotFound),
            PersistenceError::QueryFailed(_)
        ));
    }
}

#[cfg(test)]
mod data_sql_scanners_proptests {
    use super::*;
    use proptest::prelude::*;
    use sqlparser::dialect::{MySqlDialect, PostgreSqlDialect, SQLiteDialect};

    proptest! {
        #[test]
        fn validate_managed_transaction_sql_never_panics_on_arbitrary_utf8(s in "\\PC*") {
            let _ = validate_managed_transaction_sql(&s, &PostgreSqlDialect {});
            let _ = validate_managed_transaction_sql(&s, &MySqlDialect {});
            let _ = validate_managed_transaction_sql(&s, &SQLiteDialect {});
        }

        #[test]
        fn validate_managed_transaction_sql_always_rejects_unquoted_tx_control(
            prefix in prop::sample::select(vec![
                "",
                "SELECT 1; ",
                "INSERT INTO t VALUES (1); /* ok */ ",
                "-- comment\n",
            ]),
            kw in prop::sample::select(vec![
                "COMMIT",
                "commit",
                "CoMmIt",
                "ROLLBACK",
                "rollback",
                "BEGIN",
                "begin",
                "START TRANSACTION",
                "start transaction",
                "END",
                "ABORT",
                "PREPARE TRANSACTION 'tx1'",
            ]),
            suffix in prop::sample::select(vec!["", ";", "; SELECT 2;"]),
        ) {
            let sql = format!("{prefix}{kw}{suffix}");
            let pg = PostgreSqlDialect {};
            let mysql = MySqlDialect {};
            let sqlite = SQLiteDialect {};
            for (name, dialect) in [
                ("postgres", &pg as &dyn sqlparser::dialect::Dialect),
                ("mysql", &mysql),
                ("sqlite", &sqlite),
            ] {
                let err = validate_managed_transaction_sql(&sql, dialect)
                    .expect_err(&format!("{name} must reject '{sql}'"));
                prop_assert!(matches!(err, PersistenceError::TransactionError(_)));
            }
        }

        #[test]
        fn validate_managed_transaction_sql_permits_keywords_in_literals_comments_and_delimited_ids(
            pad in "[a-zA-Z0-9_ ;]{0,12}",
            kw in prop::sample::select(vec!["COMMIT", "ROLLBACK", "BEGIN", "START TRANSACTION"]),
        ) {
            let pg = PostgreSqlDialect {};
            let mysql = MySqlDialect {};
            let sqlite = SQLiteDialect {};

            let single_quoted = format!("SELECT '{pad} {kw} {pad}';");
            let line_commented = format!("-- {pad} {kw}\nSELECT 1;");
            let block_commented = format!("/* {pad} {kw} */ SELECT 1;");
            for dialect in [
                &pg as &dyn sqlparser::dialect::Dialect,
                &mysql,
                &sqlite,
            ] {
                prop_assert!(validate_managed_transaction_sql(&single_quoted, dialect).is_ok());
                prop_assert!(validate_managed_transaction_sql(&line_commented, dialect).is_ok());
                prop_assert!(validate_managed_transaction_sql(&block_commented, dialect).is_ok());
            }

            let mysql_hash = format!("# {pad} {kw}\nSELECT 1;");
            let mysql_backtick = format!("SELECT `{kw}` FROM t;");
            prop_assert!(validate_managed_transaction_sql(&mysql_hash, &mysql).is_ok());
            prop_assert!(validate_managed_transaction_sql(&mysql_backtick, &mysql).is_ok());

            let sqlite_bracket = format!("SELECT [{kw}] FROM t;");
            prop_assert!(validate_managed_transaction_sql(&sqlite_bracket, &sqlite).is_ok());

            let pg_dollar = format!("DO $$ {pad} {kw} {pad} $$;");
            let pg_double_quote = format!("SELECT \"{kw}\" FROM t;");
            prop_assert!(validate_managed_transaction_sql(&pg_dollar, &pg).is_ok());
            prop_assert!(validate_managed_transaction_sql(&pg_double_quote, &pg).is_ok());
        }
    }

    #[test]
    fn validate_managed_transaction_sql_allows_compound_bodies_and_rejects_trailing_tx_control() {
        let sqlite = SQLiteDialect {};
        let pg = PostgreSqlDialect {};

        let sqlite_trigger = r#"
            CREATE TRIGGER trg_audit AFTER INSERT ON items
            BEGIN
                INSERT INTO audit(msg) VALUES (CASE WHEN NEW.id > 0 THEN 'pos' ELSE 'neg' END);
                UPDATE stats SET cnt = cnt + 1;
            END;
        "#;
        assert!(
            validate_managed_transaction_sql(sqlite_trigger, &sqlite).is_ok(),
            "valid SQLite CREATE TRIGGER body must be accepted"
        );
        let sqlite_trigger_then_commit = format!("{sqlite_trigger}\nCOMMIT;");
        assert!(
            matches!(
                validate_managed_transaction_sql(&sqlite_trigger_then_commit, &sqlite),
                Err(PersistenceError::TransactionError(_))
            ),
            "trailing COMMIT after SQLite trigger body must be rejected"
        );

        let pg_atomic_fn = r#"
            CREATE OR REPLACE FUNCTION add_one(x INT) RETURNS INT
            LANGUAGE SQL
            BEGIN ATOMIC
                SELECT x + 1;
            END;
        "#;
        assert!(
            validate_managed_transaction_sql(pg_atomic_fn, &pg).is_ok(),
            "valid PostgreSQL BEGIN ATOMIC function body must be accepted"
        );
        let pg_fn_then_rollback = format!("{pg_atomic_fn}\nROLLBACK;");
        assert!(
            matches!(
                validate_managed_transaction_sql(&pg_fn_then_rollback, &pg),
                Err(PersistenceError::TransactionError(_))
            ),
            "trailing ROLLBACK after PostgreSQL BEGIN ATOMIC body must be rejected"
        );
    }

    #[test]
    fn validate_managed_mysql_transaction_sql_rejects_implicit_commit_ddl_and_allows_dml() {
        for ddl in [
            "CREATE TABLE t (id INT)",
            "ALTER TABLE t ADD COLUMN x INT",
            "DROP TABLE t",
            "TRUNCATE TABLE t",
            "LOCK TABLES t WRITE",
            "INSERT INTO t VALUES (1); CREATE TABLE t2 (id INT);",
        ] {
            assert!(
                matches!(
                    validate_managed_mysql_transaction_sql(ddl),
                    Err(PersistenceError::TransactionError(_))
                ),
                "MySQL implicit-commit statement must be rejected in managed tx: {ddl}"
            );
            assert!(mysql_sql_causes_implicit_commit(ddl).unwrap());
        }

        for allowed in [
            "INSERT INTO t VALUES (1); UPDATE t SET v = 2 WHERE id = 1;",
            "DELETE FROM t WHERE id = 1",
            "CREATE TEMPORARY TABLE tmp_t (id INT); DROP TEMPORARY TABLE tmp_t;",
            "SAVEPOINT sp1; ROLLBACK TO SAVEPOINT sp1;",
        ] {
            assert!(
                validate_managed_mysql_transaction_sql(allowed).is_ok(),
                "transactional MySQL DML/temp-table must be allowed: {allowed}"
            );
            assert!(!mysql_sql_causes_implicit_commit(allowed).unwrap());
        }
    }
    #[test]
    fn mysql_guard_rejects_autocommit_assignment_but_allows_lookalikes() {
        for sql in [
            "SET autocommit = 1",
            "SET `autocommit` = 1",
            "SET @@autocommit = 1",
            "SET SESSION autocommit = 1",
            "SET LOCAL autocommit = 1",
            "SET @@session.autocommit = 1",
            "SET @@session.`autocommit` = 1",
            "SET @@local.autocommit=1",
            "SET sql_mode = '', autocommit = 1",
            "SET SESSION sql_mode = '', autocommit = 1",
            "SET @@global.sort_buffer_size = 1000000, autocommit = 1",
            "SET GLOBAL sort_buffer_size = 1000000, @@session.autocommit = 1",
            "SET GLOBAL sort_buffer_size = 1000000, @@autocommit = 1",
            "SET GLOBAL sort_buffer_size = 1000000, SESSION autocommit = 1",
            "SET @saved_autocommit = @@autocommit, @@session.autocommit = 1",
            "UPDATE t SET v = 1; SET autocommit = 1;",
            "UPDATE t SET v = 1; /*! SET autocommit = 1 */;",
            "UPDATE t SET v = 1; /*!50000 SET @@session.autocommit = 1 */;",
        ] {
            assert!(
                matches!(
                    validate_managed_mysql_transaction_sql(sql),
                    Err(PersistenceError::TransactionError(_))
                ),
                "autocommit assignment must be rejected: {sql}"
            );
            assert!(
                matches!(
                    mysql_sql_causes_implicit_commit(sql),
                    Err(PersistenceError::TransactionError(_))
                ),
                "migration classifier must also reject autocommit assignment: {sql}"
            );
        }
        for ok in [
            "SELECT @@autocommit",
            "SELECT 'SET autocommit = 1'",
            "UPDATE t SET autocommit_note = 'x' WHERE id = 1",
            "UPDATE t SET v = 1 /* SET autocommit = 1 */",
            "SET @x = 1",
            "SET @autocommit = 1",
            "SET @saved_autocommit = @@autocommit",
            "SET @saved_autocommit = @@session.autocommit",
            "SET GLOBAL autocommit = 1",
            "SET @@global.autocommit = 1",
            "SET GLOBAL sort_buffer_size = 1000000, autocommit = 1",
            "SET PERSIST sort_buffer_size = 1000000, autocommit = 1",
        ] {
            assert!(
                validate_managed_mysql_transaction_sql(ok).is_ok(),
                "must not be flagged: {ok}"
            );
            assert!(
                !mysql_sql_causes_implicit_commit(ok).unwrap(),
                "must not be flagged as implicit commit: {ok}"
            );
        }
    }

    #[test]
    fn mysql_migration_classifier_rejects_explicit_transaction_control() {
        for sql in [
            "UPDATE t SET v = 2; COMMIT;",
            "START TRANSACTION; UPDATE t SET v = 2;",
            "UPDATE t SET v = 2; ROLLBACK;",
            "CREATE TABLE t2 (id INT); COMMIT;",
            "SET autocommit = 1; UPDATE t SET v = 2;",
            "UPDATE t SET v = v + 1; /*! COMMIT */;",
            "UPDATE t SET v = v + 1; /*!50000 COMMIT */;",
            "UPDATE t SET v = v + 1; /*!80000 ROLLBACK */;",
            "UPDATE t SET v = v + 1; /*!99999 CREATE TABLE t_future (id INT) */;",
            "/*! CREATE TABLE t_exec (id INT) */",
        ] {
            assert!(
                matches!(
                    mysql_sql_causes_implicit_commit(sql),
                    Err(PersistenceError::TransactionError(_))
                ),
                "migration script with transaction control or executable comment must be rejected: {sql}"
            );
            assert!(
                matches!(
                    validate_managed_mysql_transaction_sql(sql),
                    Err(PersistenceError::TransactionError(_))
                ),
                "managed tx guard must also reject: {sql}"
            );
        }
        assert!(
            !mysql_sql_causes_implicit_commit(
                "UPDATE t SET v = 2; /* regular /* comment */ INSERT INTO t VALUES (3);"
            )
            .unwrap()
        );
    }
}
