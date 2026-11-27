//! SQL-safety validation machinery for managed transactions and migrations.

// Every item in this module that uses `PersistenceError` is gated on a backend dialect feature
// (or `test`), so match that gate here to avoid an unused-import warning under default features.
#[cfg(any(feature = "postgres", feature = "mysql", feature = "sqlite", test))]
use crate::persistence::error::PersistenceError;

#[cfg(any(feature = "postgres", feature = "mysql", feature = "sqlite", test))]
fn check_managed_tx_control_keywords(words: &[String]) -> Result<(), PersistenceError> {
    let Some(first) = words.first().map(String::as_str) else {
        return Ok(());
    };
    let second = words.get(1).map(String::as_str);
    let third = words.get(2).map(String::as_str);

    let is_tx_control = match first {
        // Why: reject SQL-level `PREPARE`/`EXECUTE`/`DEALLOCATE` unconditionally so a raw
        // batch cannot smuggle `PREPARE stmt FROM 'COMMIT'; EXECUTE stmt;` past static
        // transaction-control validation (`RC-09`).
        "COMMIT" | "END" | "ABORT" | "BEGIN" | "PREPARE" | "EXECUTE" | "DEALLOCATE" => true,
        "START" => second == Some("TRANSACTION"),
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
/// Why: `sqlparser 0.64.0` applies nested block-comment handling when the active dialect's
/// `supports_nested_comments()` returns `true` (and blanking interior `/` and `*` bytes up to
/// the first `*/` guarantees C-style non-nested closing semantics across SQLite/MySQL without
/// relying on dialect flag defaults). Returns `Result` so callers propagate any validation
/// failure without `expect()` in production code.
#[cfg(feature = "sqlite")]
pub(crate) fn sanitize_non_nested_block_comments<'a>(
    statement: &'a str,
    dialect: &dyn sqlparser::dialect::Dialect,
) -> Result<std::borrow::Cow<'a, str>, PersistenceError> {
    sanitize_non_nested_block_comments_inner(statement, dialect, false)
}

/// Scans `statement` for C-style non-nested block comments (`/* ... */`) in SQLite/MySQL
/// dialects. When `reject_mysql_exec_comments` is `true` and `dialect` is `MySqlDialect`,
/// any `/*! ... */` executable comment is rejected before `sqlparser` tokenization.
///
/// Why: `sqlparser 0.64.0`'s `MySqlDialect` parses `/*!99999 ... */` as executable SQL tokens
/// rather than `Whitespace::MultiLineComment`, so checking at the raw comment-scanner layer
/// guarantees every `/*! ... */` (including version-gated `/*!99999 ... */` whose server-version
/// predicate cannot be resolved statically) is rejected deterministically before tokenization.
#[cfg(any(feature = "postgres", feature = "mysql", feature = "sqlite", test))]
fn sanitize_non_nested_block_comments_inner<'a>(
    statement: &'a str,
    dialect: &dyn sqlparser::dialect::Dialect,
    reject_mysql_exec_comments: bool,
) -> Result<std::borrow::Cow<'a, str>, PersistenceError> {
    if dialect.is::<sqlparser::dialect::PostgreSqlDialect>() || !statement.contains("/*") {
        return Ok(std::borrow::Cow::Borrowed(statement));
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
            // Why: On MySQL, `--` only starts a line comment when followed by whitespace/control
            // (` `, `\t`, `\r`, `\n`, `\x0b`, `\x0c`) or EOF; otherwise `--` is unary negation and
            // trailing tokens such as `--/*!50000 ; COMMIT; */` remain executable (`RC-20`).
            b'-' if bytes.get(i + 1) == Some(&b'-')
                && (!is_mysql
                    || matches!(
                        bytes.get(i + 2),
                        None | Some(b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c)
                    )) =>
            {
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
                // Why: In C-style non-nested block-comment dialects (SQLite `tokenize.c` and
                // MySQL `sql_lex.cc`), a `/*` comment opened at `i` ends unconditionally at the
                // very first `*/` at `j >= i + 2` (or EOF if unclosed). Locate that closing `*/`
                // first and blank every interior byte in `body_start..body_end` (preserving `\n`
                // for line/column spans and a leading `!` on MySQL so `/*!` is still rejected).
                // Because the closing `*/` at `j..(j + 2)` is never modified, overlapping
                // delimiters like `/* /*/; COMMIT; /* */` and nested-comment payloads like
                // `/* /* */ ; COMMIT; /* */ */` always close at the exact byte offset the
                // database engine uses.
                if reject_mysql_exec_comments && is_mysql && bytes.get(i + 2) == Some(&b'!') {
                    return Err(PersistenceError::TransactionError(
                        "MySQL executable comments (`/*! ... */`) are not allowed in managed transactions or migrations"
                            .into(),
                    ));
                }
                let body_start = if is_mysql && bytes.get(i + 2) == Some(&b'!') {
                    i + 3
                } else {
                    i + 2
                };
                let mut close_idx = None;
                let mut j = i + 2;
                while j + 1 < bytes.len() {
                    if bytes[j] == b'*' && bytes[j + 1] == b'/' {
                        close_idx = Some(j);
                        break;
                    }
                    j += 1;
                }
                let body_end = close_idx.unwrap_or(bytes.len());
                if body_start < body_end
                    && (bytes[body_start..body_end].contains(&b'/')
                        || bytes[body_start..body_end].contains(&b'*'))
                {
                    let buf = out.get_or_insert_with(|| bytes.to_vec());
                    for b_ref in &mut buf[body_start..body_end] {
                        if *b_ref != b'\n' {
                            *b_ref = b' ';
                        }
                    }
                }
                i = match close_idx {
                    Some(j) => j + 2,
                    None => bytes.len(),
                };
            }
            _ => {
                i += 1;
            }
        }
    }

    Ok(match out {
        Some(vec) => std::borrow::Cow::Owned(String::from_utf8(vec).expect(
            "replacing non-newline bytes inside block comment with ASCII ' ' preserves valid UTF-8",
        )),
        None => std::borrow::Cow::Borrowed(statement),
    })
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

    let sanitized = sanitize_non_nested_block_comments_inner(statement, dialect, is_mysql)?;
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
) -> Result<Vec<Vec<String>>, PersistenceError> {
    use sqlparser::tokenizer::Token;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum DefinerState {
        None,
        ExpectEq,
        ExpectUser,
        AfterUser,
        ExpectHost,
        AfterCurrentUser,
        InCurrentUserParens,
    }

    let mut heads: Vec<Vec<String>> = Vec::new();
    let mut leading_words: Vec<String> = Vec::with_capacity(3);
    let mut in_compound_create_decl = false;
    let mut seen_non_compound_create_target = false;
    let mut seen_lparen = false;
    let mut definer_state = DefinerState::None;
    let mut block_depth: usize = 0;

    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i] {
            Token::SemiColon => {
                if block_depth == 0 {
                    if !leading_words.is_empty() {
                        heads.push(std::mem::take(&mut leading_words));
                    }
                    in_compound_create_decl = false;
                    seen_non_compound_create_target = false;
                    seen_lparen = false;
                    definer_state = DefinerState::None;
                }
            }
            Token::Eq if block_depth == 0 && definer_state == DefinerState::ExpectEq => {
                definer_state = DefinerState::ExpectUser;
            }
            Token::AtSign if block_depth == 0 && definer_state == DefinerState::AfterUser => {
                definer_state = DefinerState::ExpectHost;
            }
            Token::SingleQuotedString(_) | Token::DoubleQuotedString(_)
                if block_depth == 0
                    && matches!(
                        definer_state,
                        DefinerState::ExpectUser | DefinerState::ExpectHost
                    ) =>
            {
                definer_state = if definer_state == DefinerState::ExpectUser {
                    DefinerState::AfterUser
                } else {
                    DefinerState::None
                };
            }
            Token::LParen => {
                if block_depth == 0 {
                    if definer_state == DefinerState::AfterCurrentUser {
                        definer_state = DefinerState::InCurrentUserParens;
                    } else {
                        definer_state = DefinerState::None;
                        seen_lparen = true;
                    }
                }
            }
            Token::RParen
                if block_depth == 0 && definer_state == DefinerState::InCurrentUserParens =>
            {
                definer_state = DefinerState::None;
            }
            Token::Word(word) if word.quote_style.is_none() => {
                let upper = word.value.to_ascii_uppercase();
                if block_depth == 0 {
                    if leading_words.len() < 3 {
                        leading_words.push(upper.clone());
                    }
                    // Why: handle MySQL `CREATE DEFINER = root@localhost TRIGGER` and
                    // `CREATE DEFINER = CURRENT_USER() TRIGGER` without confusing the
                    // definer user/host/parens for a non-compound DDL target or table
                    // column list (`P3-04`).
                    let consumed_by_definer = match definer_state {
                        DefinerState::ExpectUser => {
                            definer_state = if upper == "CURRENT_USER" {
                                DefinerState::AfterCurrentUser
                            } else {
                                DefinerState::AfterUser
                            };
                            true
                        }
                        DefinerState::ExpectHost => {
                            definer_state = DefinerState::None;
                            true
                        }
                        DefinerState::AfterUser | DefinerState::AfterCurrentUser => {
                            definer_state = DefinerState::None;
                            false
                        }
                        _ => false,
                    };

                    // Why: only enter a compound SQL block (`BEGIN ... END`) when the
                    // enclosing `CREATE` statement declares a compound object (`TRIGGER`,
                    // `FUNCTION`, `PROCEDURE`, or `EVENT`) BEFORE the first `(` and with
                    // only valid pre-target `CREATE` modifiers in between (allowlist, not
                    // denylist), so `CREATE DATABASE trigger BEGIN ...` or `CREATE TABLE
                    // trigger (...) BEGIN ...` cannot enable compound blocks (`RC-13`, `P3-04`).
                    if !consumed_by_definer
                        && !seen_lparen
                        && !in_compound_create_decl
                        && !seen_non_compound_create_target
                        && leading_words.len() > 1
                        && leading_words.first().map(String::as_str) == Some("CREATE")
                    {
                        if matches!(
                            upper.as_str(),
                            "TRIGGER" | "FUNCTION" | "PROCEDURE" | "EVENT"
                        ) {
                            in_compound_create_decl = true;
                        } else if upper == "DEFINER" {
                            definer_state = DefinerState::ExpectEq;
                        } else if !matches!(
                            upper.as_str(),
                            "OR" | "REPLACE" | "TEMP" | "TEMPORARY" | "CONSTRAINT"
                        ) {
                            seen_non_compound_create_target = true;
                        }
                    }
                    if upper == "BEGIN" && in_compound_create_decl {
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

    // Why: fail closed if a compound `BEGIN` block is still open at EOF so an
    // attacker cannot leave `block_depth > 0` to hide trailing `; COMMIT;` (`RC-13`).
    if block_depth > 0 {
        return Err(PersistenceError::TransactionError(
            "unclosed compound SQL block (`BEGIN` without matching `END`) is not allowed inside a managed transaction"
                .into(),
        ));
    }

    if !leading_words.is_empty() {
        heads.push(leading_words);
    }

    Ok(heads)
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
/// Why: Delegate SQL lexing and AST inspection to `sqlparser = "=0.64.0"` instead of maintaining
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
    for words in collect_top_level_statement_heads_from_tokens(&tokens)? {
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
        | "UNINSTALL" | "XA" | "STOP" | "CHANGE" | "IMPORT" | "CLONE" | "SHUTDOWN" => true,
        "START" => matches!(second, Some("REPLICA" | "SLAVE" | "GROUP_REPLICATION")),
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

    let heads = collect_top_level_statement_heads_from_tokens(&tokens)?;
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

    let heads = collect_top_level_statement_heads_from_tokens(&tokens)?;
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

/// Fuzzing-only entry points exposing the managed transaction SQL validators without widening
/// the normal public API (`fuzz/` targets only, gated behind `feature = "fuzzing"`).
#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub mod fuzz_support {
    use super::{
        mysql_sql_causes_implicit_commit, validate_managed_mysql_transaction_sql,
        validate_managed_transaction_sql,
    };

    /// Fuzz entry point for PostgreSQL, SQLite, and MySQL managed transaction validators.
    pub fn fuzz_validate_sql_validators(statement: &str) {
        let _ =
            validate_managed_transaction_sql(statement, &sqlparser::dialect::PostgreSqlDialect {});
        let _ = validate_managed_transaction_sql(statement, &sqlparser::dialect::SQLiteDialect {});
        let _ = validate_managed_mysql_transaction_sql(statement);
        let _ = mysql_sql_causes_implicit_commit(statement);
        let _ = crate::persistence::sqlite::validate_sqlite_placeholders(statement, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // These traits were previously in scope via `use super::*` in backend.rs's own `mod tests`
    // (the traits lived in that module); the split moves the test into `validation`, so import
    // them explicitly for the `#[cfg(feature = "sqlite")]` integration-style assertions.
    #[cfg(feature = "sqlite")]
    use crate::persistence::backend::{DatabaseBackend, DatabaseTransaction};

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
            for sql in [
                "COMMIT",
                "ROLLBACK",
                "BEGIN",
                "START TRANSACTION",
                "CREATE TABLE t (x INT) BEGIN; COMMIT;",
                "CREATE TRIGGER t BEFORE INSERT ON foo BEGIN INSERT INTO log VALUES (1); COMMIT;",
            ] {
                let err = validate_managed_transaction_sql(sql, dialect)
                    .expect_err(&format!("{name} must reject raw '{sql}'"));
                assert!(
                    matches!(err, PersistenceError::TransactionError(_)),
                    "{name} expected TransactionError for '{sql}', got: {err:?}"
                );
            }
        }
        assert!(matches!(
            validate_managed_mysql_transaction_sql("CREATE TABLE t (x INT) BEGIN; COMMIT;"),
            Err(PersistenceError::TransactionError(_))
        ));
        assert!(matches!(
            validate_managed_mysql_transaction_sql(
                "CREATE TRIGGER t BEFORE INSERT ON foo BEGIN INSERT INTO log VALUES (1); COMMIT;"
            ),
            Err(PersistenceError::TransactionError(_))
        ));

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

    #[test]
    fn mysql_validator_rejects_exec_comment_after_non_whitespace_double_dash() {
        for bypass in [
            "SELECT 1--/*!50000 ; COMMIT; */\n",
            "SELECT 1--/*!99999 SELECT 1 */\n",
        ] {
            assert!(
                matches!(
                    validate_managed_mysql_transaction_sql(bypass),
                    Err(PersistenceError::TransactionError(_))
                ),
                "MySQL validator must reject executable comment after `--` not followed by whitespace: {bypass}"
            );
            assert!(
                matches!(
                    mysql_sql_causes_implicit_commit(bypass),
                    Err(PersistenceError::TransactionError(_))
                ),
                "MySQL implicit-commit classifier must reject executable comment after `--` not followed by whitespace: {bypass}"
            );
        }

        // Real MySQL `-- ` line comments (followed by whitespace/control or EOF) still work.
        for ok in [
            "SELECT 1 -- /*!50000 ; COMMIT; */\n",
            "SELECT 1 --\t/*!50000 ; COMMIT; */\n",
            "SELECT 1 --",
        ] {
            assert!(
                validate_managed_mysql_transaction_sql(ok).is_ok(),
                "valid MySQL line comment must be allowed: {ok}"
            );
        }
    }

    #[test]
    fn create_table_column_named_event_does_not_enable_compound_begin_block() {
        // `CREATE TABLE t (event INT)` is valid DDL by itself.
        assert!(
            validate_managed_transaction_sql(
                "CREATE TABLE t (event INT);",
                &sqlparser::dialect::SQLiteDialect {},
            )
            .is_ok()
        );
        // However, `event` inside `(...)` must NOT set `in_compound_create_decl` so a
        // trailing `BEGIN ... ; COMMIT; END;` cannot be hidden inside it.
        let smuggled = "CREATE TABLE t (event INT) BEGIN SELECT 1; COMMIT; END;";
        let res = validate_managed_transaction_sql(smuggled, &sqlparser::dialect::SQLiteDialect {});
        assert!(
            matches!(res, Err(PersistenceError::TransactionError(_))),
            "column name `event` inside CREATE TABLE (...) must not enable compound BEGIN block"
        );

        // `P2-20` & `P3-04`: non-compound CREATE targets (including unlisted DDL targets like
        // `DATABASE` / `ROLE`) must NOT enable compound BEGIN blocks, while valid MySQL
        // `CREATE DEFINER = ... TRIGGER` declarations remain allowed.
        for bypass in [
            "CREATE TABLE trigger (id INT) BEGIN SELECT 1; COMMIT; END;",
            "CREATE VIEW event AS SELECT 1 BEGIN SELECT 1; ROLLBACK; END;",
            "CREATE DATABASE trigger BEGIN SELECT 1; COMMIT; END;",
            "CREATE ROLE event BEGIN SELECT 1; COMMIT; END;",
        ] {
            let res =
                validate_managed_transaction_sql(bypass, &sqlparser::dialect::SQLiteDialect {});
            assert!(
                matches!(res, Err(PersistenceError::TransactionError(_))),
                "non-compound CREATE target must not enable compound BEGIN block: {bypass}"
            );
        }

        for valid_definer_trigger in [
            "CREATE DEFINER = root@localhost TRIGGER t_ins AFTER INSERT ON items FOR EACH ROW BEGIN INSERT INTO log VALUES (1); END;",
            "CREATE DEFINER = CURRENT_USER() TRIGGER t_ins AFTER INSERT ON items FOR EACH ROW BEGIN INSERT INTO log VALUES (1); END;",
        ] {
            assert!(
                validate_managed_transaction_sql(
                    valid_definer_trigger,
                    &sqlparser::dialect::MySqlDialect {}
                )
                .is_ok(),
                "valid MySQL DEFINER trigger must be allowed: {valid_definer_trigger}"
            );
        }
    }

    #[test]
    fn rejects_mysql_implicit_commit_admin_statements_and_dynamic_sql_prepare_execute() {
        // MySQL 8.4 §15.3.3 implicit-commit replication/admin statements (`RC-09`).
        for stmt in [
            "STOP REPLICA;",
            "STOP SLAVE;",
            "CHANGE REPLICATION SOURCE TO SOURCE_HOST='127.0.0.1';",
            "IMPORT TABLE FROM '/tmp/t.sdi';",
            "CLONE LOCAL DATA DIRECTORY = '/tmp/clone';",
            "SHUTDOWN;",
            "START REPLICA;",
            "START SLAVE;",
            "START GROUP_REPLICATION;",
        ] {
            assert!(
                matches!(
                    validate_managed_mysql_transaction_sql(stmt),
                    Err(PersistenceError::TransactionError(_))
                ),
                "expected TransactionError in managed MySQL tx for: {stmt}"
            );
            assert!(
                mysql_sql_causes_implicit_commit(stmt).expect("classify migration"),
                "expected mysql_sql_causes_implicit_commit == true for: {stmt}"
            );
        }

        // Dynamic SQL prepared statements (`PREPARE`/`EXECUTE`/`DEALLOCATE`) blocked across dialects (`RC-09`).
        for stmt in [
            "PREPARE stmt FROM 'COMMIT';",
            "EXECUTE stmt;",
            "DEALLOCATE PREPARE stmt;",
        ] {
            assert!(
                matches!(
                    validate_managed_mysql_transaction_sql(stmt),
                    Err(PersistenceError::TransactionError(_))
                ),
                "expected MySQL TransactionError for: {stmt}"
            );
            assert!(
                matches!(
                    mysql_sql_causes_implicit_commit(stmt),
                    Err(PersistenceError::TransactionError(_))
                ),
                "expected MySQL migration error for: {stmt}"
            );
            assert!(
                matches!(
                    validate_managed_transaction_sql(
                        stmt,
                        &sqlparser::dialect::PostgreSqlDialect {}
                    ),
                    Err(PersistenceError::TransactionError(_))
                ),
                "expected Postgres TransactionError for: {stmt}"
            );
        }
    }
}
