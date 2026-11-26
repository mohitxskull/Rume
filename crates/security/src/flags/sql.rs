//! SQL-backed [`FeatureFlagStore`] across SQLite, PostgreSQL, and MySQL.

use std::sync::Arc;

use rume_data::persistence::{DatabaseBackend, DbRow, DbValue};
use rume_data::sql;

use super::eval::{
    EvaluationContext, FeatureFlag, RolloutPercentage, TargetingRule, evaluate_flag,
};
use super::store::{BoxFuture, FeatureFlagStore, FlagError};

fn select_sql(
    dialect: &str,
    pg: &'static str,
    mysql: &'static str,
    sqlite: &'static str,
) -> &'static str {
    match dialect {
        "postgres" => pg,
        "mysql" => mysql,
        _ => sqlite,
    }
}

fn composite_id(tenant_id: Option<&str>, key: &str) -> String {
    // Why: SQL UNIQUE indexes treat NULLs as distinct, so global flags
    // (`tenant_id IS NULL`) use a deterministic `"__global__:<key>"` primary
    // key to guarantee idempotent upserts across SQLite, Postgres, and MySQL.
    match tenant_id {
        Some(tid) => format!("{tid}:{key}"),
        None => format!("__global__:{key}"),
    }
}

fn decode_flag_row(row: &DbRow) -> Result<FeatureFlag, FlagError> {
    let map_err = |e: rume_data::persistence::PersistenceError| FlagError::Store(e.to_string());
    let key: String = row.get("flag_key").map_err(map_err)?;
    let tenant_id: Option<String> = row.get_opt("tenant_id").map_err(map_err)?;
    let enabled: bool = row.get("enabled").map_err(map_err)?;
    let rollout_pct: i64 = row.get("rollout_pct").map_err(map_err)?;
    let rules_json: Option<String> = row.get_opt("rules").map_err(map_err)?;

    let rules: Vec<TargetingRule> = match rules_json {
        Some(raw) if !raw.trim().is_empty() => serde_json::from_str(&raw)
            .map_err(|e| FlagError::Serialization(format!("invalid rules JSON: {e}")))?,
        _ => Vec::new(),
    };

    let pct = u8::try_from(rollout_pct.clamp(0, 100)).unwrap_or(100);
    Ok(FeatureFlag {
        key,
        tenant_id,
        enabled,
        rollout: RolloutPercentage::new(pct),
        rules,
    })
}

/// Persistent SQL [`FeatureFlagStore`] across SQLite, PostgreSQL, and MySQL.
#[derive(Debug, Clone)]
pub struct SqlFeatureFlags<B: DatabaseBackend> {
    backend: Arc<B>,
}

impl<B: DatabaseBackend> SqlFeatureFlags<B> {
    /// Creates a new [`SqlFeatureFlags`] backed by `backend`.
    pub fn new(backend: Arc<B>) -> Self {
        Self { backend }
    }

    /// Ensures the `rume_feature_flags` schema exists on the underlying backend.
    pub async fn ensure_schema(&self) -> Result<(), FlagError> {
        let ddl = select_sql(
            self.backend.dialect(),
            sql::PG_FEATURE_FLAGS_SCHEMA,
            sql::MYSQL_FEATURE_FLAGS_SCHEMA,
            sql::SQLITE_FEATURE_FLAGS_SCHEMA,
        );
        self.backend
            .execute(ddl)
            .await
            .map(|_| ())
            .map_err(|e| FlagError::Store(e.to_string()))
    }
}

impl<B: DatabaseBackend> FeatureFlagStore for SqlFeatureFlags<B> {
    fn upsert_flag<'a>(&'a self, flag: FeatureFlag) -> BoxFuture<'a, Result<(), FlagError>> {
        Box::pin(async move {
            let id = composite_id(flag.tenant_id.as_deref(), &flag.key);
            let rules_json = serde_json::to_string(&flag.rules)
                .map_err(|e| FlagError::Serialization(e.to_string()))?;
            let stmt = select_sql(
                self.backend.dialect(),
                sql::PG_FEATURE_FLAGS_UPSERT,
                sql::MYSQL_FEATURE_FLAGS_UPSERT,
                sql::SQLITE_FEATURE_FLAGS_UPSERT,
            );
            let params = [
                DbValue::from(id),
                DbValue::from(flag.tenant_id.as_deref()),
                DbValue::from(flag.key.as_str()),
                DbValue::from(flag.enabled),
                DbValue::from(i64::from(flag.rollout.value())),
                DbValue::from(rules_json),
            ];
            self.backend
                .execute_params(stmt, &params)
                .await
                .map(|_| ())
                .map_err(|e| FlagError::Store(e.to_string()))
        })
    }

    fn get_flag<'a>(
        &'a self,
        key: &'a str,
        tenant_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Option<FeatureFlag>, FlagError>> {
        Box::pin(async move {
            let id = composite_id(tenant_id, key);
            let stmt = select_sql(
                self.backend.dialect(),
                sql::PG_FEATURE_FLAGS_GET,
                sql::MYSQL_FEATURE_FLAGS_GET,
                sql::SQLITE_FEATURE_FLAGS_GET,
            );
            let row = self
                .backend
                .fetch_optional(stmt, &[DbValue::from(id)])
                .await
                .map_err(|e| FlagError::Store(e.to_string()))?;
            row.as_ref().map(decode_flag_row).transpose()
        })
    }

    fn list_flags<'a>(
        &'a self,
        tenant_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<FeatureFlag>, FlagError>> {
        Box::pin(async move {
            let rows = match tenant_id {
                Some(tid) => {
                    let stmt = select_sql(
                        self.backend.dialect(),
                        sql::PG_FEATURE_FLAGS_LIST_TENANT,
                        sql::MYSQL_FEATURE_FLAGS_LIST_TENANT,
                        sql::SQLITE_FEATURE_FLAGS_LIST_TENANT,
                    );
                    self.backend
                        .fetch_all(stmt, &[DbValue::from(tid)])
                        .await
                        .map_err(|e| FlagError::Store(e.to_string()))?
                }
                None => {
                    let stmt = select_sql(
                        self.backend.dialect(),
                        sql::PG_FEATURE_FLAGS_LIST_GLOBAL,
                        sql::MYSQL_FEATURE_FLAGS_LIST_GLOBAL,
                        sql::SQLITE_FEATURE_FLAGS_LIST_GLOBAL,
                    );
                    self.backend
                        .fetch_all(stmt, &[])
                        .await
                        .map_err(|e| FlagError::Store(e.to_string()))?
                }
            };
            rows.iter().map(decode_flag_row).collect()
        })
    }

    fn delete_flag<'a>(
        &'a self,
        key: &'a str,
        tenant_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<bool, FlagError>> {
        Box::pin(async move {
            let id = composite_id(tenant_id, key);
            let stmt = select_sql(
                self.backend.dialect(),
                sql::PG_FEATURE_FLAGS_DELETE,
                sql::MYSQL_FEATURE_FLAGS_DELETE,
                sql::SQLITE_FEATURE_FLAGS_DELETE,
            );
            let affected = self
                .backend
                .execute_params(stmt, &[DbValue::from(id)])
                .await
                .map_err(|e| FlagError::Store(e.to_string()))?;
            Ok(affected > 0)
        })
    }

    fn evaluate<'a>(
        &'a self,
        key: &'a str,
        ctx: &'a EvaluationContext,
    ) -> BoxFuture<'a, Result<bool, FlagError>> {
        Box::pin(async move {
            if let Some(tid) = ctx.tenant_id.as_deref()
                && let Some(flag) = self.get_flag(key, Some(tid)).await?
            {
                return Ok(evaluate_flag(&flag, ctx));
            }
            if let Some(flag) = self.get_flag(key, None).await? {
                return Ok(evaluate_flag(&flag, ctx));
            }
            Ok(false)
        })
    }
}
