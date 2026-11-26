//! Dynamic SQL query builder backed by `sea-query`.
//!
//! Provides dialect-aware typed wrappers around `sea_query` statements that compile
//! into parameterized [`BuiltQuery`] values with [`DbValue`] bindings (zero raw string
//! concatenation).

use sea_query::{
    DeleteStatement, InsertStatement, MysqlQueryBuilder, PostgresQueryBuilder, SelectStatement,
    SqliteQueryBuilder, UpdateStatement, Value as SeaValue, Values as SeaValues,
};

use crate::persistence::backend::{DatabaseBackend, DatabaseTransaction, DbValue};
use crate::persistence::error::PersistenceError;
use crate::persistence::pool::DatabasePool;
use crate::persistence::row::{DbRow, FromRow};

/// Supported SQL dialects for compiling dynamic `sea-query` statements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SqlDialect {
    /// SQLite dialect (`"..."` identifiers, `?` placeholders).
    Sqlite,
    /// PostgreSQL dialect (`"..."` identifiers, `$1, $2, ...` placeholders).
    Postgres,
    /// MySQL / MariaDB dialect (`` `...` `` identifiers, `?` placeholders).
    MySql,
}

impl SqlDialect {
    /// Parses a dialect name matching [`DatabaseBackend::dialect`] into a [`SqlDialect`].
    pub fn from_str_name(dialect: &str) -> Result<Self, PersistenceError> {
        match dialect.to_ascii_lowercase().as_str() {
            "sqlite" => Ok(Self::Sqlite),
            "postgres" | "postgresql" => Ok(Self::Postgres),
            "mysql" | "mariadb" => Ok(Self::MySql),
            _ => Err(PersistenceError::DialectMismatch {
                query: "SqlDialect".to_string(),
                expected: "sqlite | postgres | mysql".to_string(),
                actual: dialect.to_string(),
            }),
        }
    }

    /// Returns the canonical lowercase dialect name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::Postgres => "postgres",
            Self::MySql => "mysql",
        }
    }
}

/// A compiled, parameterized SQL statement ready for execution via [`DatabasePool`] or
/// [`DatabaseTransaction`].
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltQuery {
    /// Parameterized SQL string with dialect-appropriate placeholders.
    pub sql: String,
    /// Ordered parameter bindings corresponding to the placeholders in [`Self::sql`].
    pub params: Vec<DbValue>,
}

impl BuiltQuery {
    /// Creates a new [`BuiltQuery`] from a SQL string and bound parameters.
    pub fn new(sql: impl Into<String>, params: Vec<DbValue>) -> Self {
        Self {
            sql: sql.into(),
            params,
        }
    }

    /// Executes the statement against a [`DatabasePool`], returning affected row count.
    pub async fn execute<B: DatabaseBackend>(
        &self,
        pool: &DatabasePool<B>,
    ) -> Result<u64, PersistenceError> {
        pool.execute_params(&self.sql, &self.params).await
    }

    /// Executes the statement within a [`DatabaseTransaction`], returning affected row count.
    pub async fn execute_tx<T: DatabaseTransaction>(
        &self,
        tx: &T,
    ) -> Result<u64, PersistenceError> {
        tx.execute_params(&self.sql, &self.params).await
    }

    /// Fetches all matching rows as generic [`DbRow`] records from a [`DatabasePool`].
    pub async fn fetch_all<B: DatabaseBackend>(
        &self,
        pool: &DatabasePool<B>,
    ) -> Result<Vec<DbRow>, PersistenceError> {
        pool.fetch_all(&self.sql, &self.params).await
    }

    /// Fetches all matching rows as generic [`DbRow`] records within a [`DatabaseTransaction`].
    pub async fn fetch_all_tx<T: DatabaseTransaction>(
        &self,
        tx: &T,
    ) -> Result<Vec<DbRow>, PersistenceError> {
        tx.fetch_all(&self.sql, &self.params).await
    }

    /// Fetches all matching rows from a [`DatabasePool`] and maps each into `R` via [`FromRow`].
    pub async fn fetch_all_as<B: DatabaseBackend, R: FromRow>(
        &self,
        pool: &DatabasePool<B>,
    ) -> Result<Vec<R>, PersistenceError> {
        pool.fetch_all_as(&self.sql, &self.params).await
    }

    /// Fetches all matching rows within a [`DatabaseTransaction`] and maps each into `R` via [`FromRow`].
    pub async fn fetch_all_tx_as<T: DatabaseTransaction, R: FromRow>(
        &self,
        tx: &T,
    ) -> Result<Vec<R>, PersistenceError> {
        let rows = tx.fetch_all(&self.sql, &self.params).await?;
        rows.iter().map(R::from_row).collect()
    }

    /// Fetches an optional single row as [`DbRow`] from a [`DatabasePool`].
    pub async fn fetch_optional<B: DatabaseBackend>(
        &self,
        pool: &DatabasePool<B>,
    ) -> Result<Option<DbRow>, PersistenceError> {
        pool.fetch_optional(&self.sql, &self.params).await
    }

    /// Fetches an optional single row as [`DbRow`] within a [`DatabaseTransaction`].
    pub async fn fetch_optional_tx<T: DatabaseTransaction>(
        &self,
        tx: &T,
    ) -> Result<Option<DbRow>, PersistenceError> {
        tx.fetch_optional(&self.sql, &self.params).await
    }

    /// Fetches an optional single row from a [`DatabasePool`] and maps it into `R` via [`FromRow`].
    pub async fn fetch_optional_as<B: DatabaseBackend, R: FromRow>(
        &self,
        pool: &DatabasePool<B>,
    ) -> Result<Option<R>, PersistenceError> {
        pool.fetch_optional_as(&self.sql, &self.params).await
    }

    /// Fetches an optional single row within a [`DatabaseTransaction`] and maps it into `R` via [`FromRow`].
    pub async fn fetch_optional_tx_as<T: DatabaseTransaction, R: FromRow>(
        &self,
        tx: &T,
    ) -> Result<Option<R>, PersistenceError> {
        match tx.fetch_optional(&self.sql, &self.params).await? {
            Some(row) => R::from_row(&row).map(Some),
            None => Ok(None),
        }
    }

    /// Fetches exactly one row as [`DbRow`] from a [`DatabasePool`].
    pub async fn fetch_one<B: DatabaseBackend>(
        &self,
        pool: &DatabasePool<B>,
    ) -> Result<DbRow, PersistenceError> {
        pool.fetch_one(&self.sql, &self.params).await
    }

    /// Fetches exactly one row as [`DbRow`] within a [`DatabaseTransaction`].
    pub async fn fetch_one_tx<T: DatabaseTransaction>(
        &self,
        tx: &T,
    ) -> Result<DbRow, PersistenceError> {
        tx.fetch_one(&self.sql, &self.params).await
    }

    /// Fetches exactly one row from a [`DatabasePool`] and maps it into `R` via [`FromRow`].
    pub async fn fetch_one_as<B: DatabaseBackend, R: FromRow>(
        &self,
        pool: &DatabasePool<B>,
    ) -> Result<R, PersistenceError> {
        pool.fetch_one_as(&self.sql, &self.params).await
    }

    /// Fetches exactly one row within a [`DatabaseTransaction`] and maps it into `R` via [`FromRow`].
    pub async fn fetch_one_tx_as<T: DatabaseTransaction, R: FromRow>(
        &self,
        tx: &T,
    ) -> Result<R, PersistenceError> {
        let row = tx.fetch_one(&self.sql, &self.params).await?;
        R::from_row(&row)
    }

    /// Executes a scalar query against a [`DatabasePool`] and returns the first column as an optional string.
    pub async fn query_scalar<B: DatabaseBackend>(
        &self,
        pool: &DatabasePool<B>,
    ) -> Result<Option<String>, PersistenceError> {
        pool.query_scalar_params(&self.sql, &self.params).await
    }

    /// Executes a scalar query within a [`DatabaseTransaction`] and returns the first column as an optional string.
    pub async fn query_scalar_tx<T: DatabaseTransaction>(
        &self,
        tx: &T,
    ) -> Result<Option<String>, PersistenceError> {
        tx.query_scalar_params(&self.sql, &self.params).await
    }
}

fn sea_values_to_db_values(values: SeaValues) -> Vec<DbValue> {
    values.0.into_iter().map(sea_value_to_db_value).collect()
}

fn sea_value_to_db_value(value: SeaValue) -> DbValue {
    match value {
        SeaValue::Bool(Some(b)) => DbValue::Bool(b),
        SeaValue::TinyInt(Some(n)) => DbValue::from(n),
        SeaValue::SmallInt(Some(n)) => DbValue::from(n),
        SeaValue::Int(Some(n)) => DbValue::from(n),
        SeaValue::BigInt(Some(n)) => DbValue::from(n),
        SeaValue::TinyUnsigned(Some(n)) => DbValue::from(n),
        SeaValue::SmallUnsigned(Some(n)) => DbValue::from(n),
        SeaValue::Unsigned(Some(n)) => DbValue::from(n),
        SeaValue::BigUnsigned(Some(n)) => DbValue::from(n),
        SeaValue::Float(Some(n)) => DbValue::from(n),
        SeaValue::Double(Some(n)) => DbValue::from(n),
        SeaValue::String(Some(s)) => DbValue::Text(s),
        SeaValue::Enum(sea_query::OptionEnum::Some(v)) => DbValue::Text(v.value.to_string()),
        SeaValue::Char(Some(c)) => DbValue::Text(c.to_string()),
        SeaValue::Bytes(Some(b)) => DbValue::Bytes(b),
        SeaValue::Bool(None)
        | SeaValue::TinyInt(None)
        | SeaValue::SmallInt(None)
        | SeaValue::Int(None)
        | SeaValue::BigInt(None)
        | SeaValue::TinyUnsigned(None)
        | SeaValue::SmallUnsigned(None)
        | SeaValue::Unsigned(None)
        | SeaValue::BigUnsigned(None)
        | SeaValue::Float(None)
        | SeaValue::Double(None)
        | SeaValue::String(None)
        | SeaValue::Enum(sea_query::OptionEnum::None(_))
        | SeaValue::Char(None)
        | SeaValue::Bytes(None) => DbValue::Null,
    }
}

macro_rules! impl_dynamic_statement {
    ($wrapper:ident, $stmt:ty, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone)]
        pub struct $wrapper {
            inner: $stmt,
        }

        impl $wrapper {
            /// Wraps a `sea-query` statement.
            pub fn new(inner: $stmt) -> Self {
                Self { inner }
            }

            /// Returns an immutable reference to the underlying `sea-query` statement.
            pub fn as_inner(&self) -> &$stmt {
                &self.inner
            }

            /// Returns a mutable reference to the underlying `sea-query` statement.
            pub fn as_inner_mut(&mut self) -> &mut $stmt {
                &mut self.inner
            }

            /// Consumes the wrapper and returns the underlying `sea-query` statement.
            pub fn into_inner(self) -> $stmt {
                self.inner
            }

            /// Compiles the statement for the specified [`SqlDialect`].
            pub fn build(&self, dialect: SqlDialect) -> BuiltQuery {
                let (sql, values) = match dialect {
                    SqlDialect::Sqlite => self.inner.build(SqliteQueryBuilder),
                    SqlDialect::Postgres => self.inner.build(PostgresQueryBuilder),
                    SqlDialect::MySql => self.inner.build(MysqlQueryBuilder),
                };
                BuiltQuery::new(sql, sea_values_to_db_values(values))
            }

            /// Compiles the statement using the dialect reported by `pool`.
            pub fn build_for_pool<B: DatabaseBackend>(
                &self,
                pool: &DatabasePool<B>,
            ) -> Result<BuiltQuery, PersistenceError> {
                let dialect = SqlDialect::from_str_name(pool.backend().dialect())?;
                Ok(self.build(dialect))
            }

            /// Compiles the statement using the dialect reported by `tx`.
            pub fn build_for_tx<T: DatabaseTransaction>(
                &self,
                tx: &T,
            ) -> Result<BuiltQuery, PersistenceError> {
                let dialect = SqlDialect::from_str_name(tx.dialect())?;
                Ok(self.build(dialect))
            }
        }

        impl From<$stmt> for $wrapper {
            fn from(inner: $stmt) -> Self {
                Self::new(inner)
            }
        }
    };
}

impl_dynamic_statement!(
    DynamicSelect,
    SelectStatement,
    "Typed dynamic `SELECT` statement wrapper backed by `sea_query::SelectStatement`."
);
impl_dynamic_statement!(
    DynamicInsert,
    InsertStatement,
    "Typed dynamic `INSERT` statement wrapper backed by `sea_query::InsertStatement`."
);
impl_dynamic_statement!(
    DynamicUpdate,
    UpdateStatement,
    "Typed dynamic `UPDATE` statement wrapper backed by `sea_query::UpdateStatement`."
);
impl_dynamic_statement!(
    DynamicDelete,
    DeleteStatement,
    "Typed dynamic `DELETE` statement wrapper backed by `sea_query::DeleteStatement`."
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::{DatabaseConfig, DatabasePool, DbValue, SqliteBackend};
    use sea_query::{Alias, Expr, ExprTrait, Order, Query};

    #[test]
    fn dynamic_select_compiles_for_sqlite_postgres_and_mysql_dialects() {
        let mut stmt = Query::select();
        stmt.column(Alias::new("id"))
            .column(Alias::new("email"))
            .from(Alias::new("users"))
            .and_where(Expr::col(Alias::new("active")).eq(true))
            .and_where(Expr::col(Alias::new("age")).gte(18_i64))
            .order_by(Alias::new("id"), Order::Asc)
            .limit(10);

        let select = DynamicSelect::new(stmt);

        let sqlite = select.build(SqlDialect::Sqlite);
        assert!(sqlite.sql.contains("\"users\""));
        assert!(sqlite.sql.contains("?"));
        assert!(!sqlite.sql.contains("$1"));
        assert_eq!(
            sqlite.params,
            vec![DbValue::Bool(true), DbValue::Int(18), DbValue::Int(10)]
        );

        let postgres = select.build(SqlDialect::Postgres);
        assert!(postgres.sql.contains("\"users\""));
        assert!(postgres.sql.contains("$1"));
        assert!(postgres.sql.contains("$2"));
        assert!(postgres.sql.contains("$3"));
        assert_eq!(
            postgres.params,
            vec![DbValue::Bool(true), DbValue::Int(18), DbValue::Int(10)]
        );

        let mysql = select.build(SqlDialect::MySql);
        assert!(mysql.sql.contains("`users`"));
        assert!(mysql.sql.contains("?"));
        assert!(!mysql.sql.contains("$1"));
        assert_eq!(
            mysql.params,
            vec![DbValue::Bool(true), DbValue::Int(18), DbValue::Int(10)]
        );
    }

    #[tokio::test]
    async fn dynamic_crud_executes_against_in_memory_sqlite() {
        let backend = SqliteBackend::in_memory().await.expect("sqlite in-memory");
        let pool = DatabasePool::new(backend, DatabaseConfig::new("sqlite::memory:"));

        pool.execute(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL, qty INTEGER NOT NULL)",
        )
        .await
        .expect("create table");

        let mut insert_stmt = Query::insert();
        insert_stmt
            .into_table(Alias::new("items"))
            .columns([Alias::new("id"), Alias::new("name"), Alias::new("qty")])
            .values_panic([1_i64.into(), "widget".into(), 5_i64.into()]);
        let inserted = DynamicInsert::new(insert_stmt)
            .build(SqlDialect::Sqlite)
            .execute(&pool)
            .await
            .expect("insert");
        assert_eq!(inserted, 1);

        let mut select_stmt = Query::select();
        select_stmt
            .columns([Alias::new("id"), Alias::new("name"), Alias::new("qty")])
            .from(Alias::new("items"))
            .and_where(Expr::col(Alias::new("id")).eq(1_i64));
        let row = DynamicSelect::new(select_stmt.clone())
            .build(SqlDialect::Sqlite)
            .fetch_one(&pool)
            .await
            .expect("select");
        assert_eq!(row.get::<String>("name").expect("name"), "widget");
        assert_eq!(row.get::<i64>("qty").expect("qty"), 5);

        let mut update_stmt = Query::update();
        update_stmt
            .table(Alias::new("items"))
            .value(Alias::new("qty"), 12_i64)
            .and_where(Expr::col(Alias::new("id")).eq(1_i64));
        let updated = DynamicUpdate::new(update_stmt)
            .build(SqlDialect::Sqlite)
            .execute(&pool)
            .await
            .expect("update");
        assert_eq!(updated, 1);

        let row_after = DynamicSelect::new(select_stmt)
            .build(SqlDialect::Sqlite)
            .fetch_one(&pool)
            .await
            .expect("select after update");
        assert_eq!(row_after.get::<i64>("qty").expect("qty"), 12);

        let mut delete_stmt = Query::delete();
        delete_stmt
            .from_table(Alias::new("items"))
            .and_where(Expr::col(Alias::new("id")).eq(1_i64));
        let deleted = DynamicDelete::new(delete_stmt)
            .build(SqlDialect::Sqlite)
            .execute(&pool)
            .await
            .expect("delete");
        assert_eq!(deleted, 1);
    }

    #[test]
    fn sql_dialect_from_str_name_parses_known_and_rejects_unknown() {
        assert_eq!(
            SqlDialect::from_str_name("sqlite").unwrap(),
            SqlDialect::Sqlite
        );
        assert_eq!(
            SqlDialect::from_str_name("PostgreSQL").unwrap(),
            SqlDialect::Postgres
        );
        assert_eq!(
            SqlDialect::from_str_name("mysql").unwrap(),
            SqlDialect::MySql
        );
        assert!(SqlDialect::from_str_name("oracle").is_err());
    }
}
