//! Database seeding trait, registry, and execution report.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::persistence::backend::{BoxFuture, DatabaseBackend, DatabaseTransaction, DbValue};
use crate::persistence::error::PersistenceError;
use crate::persistence::pool::DatabasePool;

/// A single idempotent or one-shot database seeder.
pub trait Seeder: Send + Sync {
    /// Unique identifier for this seeder (stored in `_rume_seeds` and used by `dependencies`).
    fn name(&self) -> &'static str;

    /// Names of seeders that must execute before this seeder.
    fn dependencies(&self) -> &[&'static str] {
        &[]
    }

    /// Executes the seed logic inside an active database transaction.
    fn run<'a>(
        &'a self,
        tx: &'a dyn DatabaseTransaction,
    ) -> BoxFuture<'a, Result<(), PersistenceError>>;
}

/// Summary of a [`SeedRegistry`] execution run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SeedReport {
    /// Names of seeders applied during this run, in execution order.
    pub applied: Vec<String>,
    /// Names of seeders skipped because they were already recorded in `_rume_seeds`.
    pub skipped: Vec<String>,
}

/// A [`Seeder`] backed by a SQL script (e.g. a `.sql` file discovered by `rume db seed`).
#[derive(Debug, Clone)]
pub struct SqlFileSeeder {
    name: &'static str,
    sql: String,
    dependencies: Vec<&'static str>,
}

impl SqlFileSeeder {
    /// Creates a new SQL file seeder from a dynamic name and SQL content.
    pub fn new(name: impl Into<String>, sql: impl Into<String>) -> Self {
        // Why: `Seeder::name` returns `&'static str` for ergonomic static seeders, while CLI
        // discovery loads `.sql` filenames dynamically at runtime; leaking the short seed name
        // once per discovered file avoids changing the trait signature to `&str`.
        let leaked_name: &'static str = Box::leak(name.into().into_boxed_str());
        Self {
            name: leaked_name,
            sql: sql.into(),
            dependencies: Vec::new(),
        }
    }

    /// Creates a new SQL file seeder from a static name and SQL content.
    pub fn from_static(name: &'static str, sql: impl Into<String>) -> Self {
        Self {
            name,
            sql: sql.into(),
            dependencies: Vec::new(),
        }
    }

    /// Declares seeders that must run before this seeder.
    pub fn with_dependencies(mut self, dependencies: Vec<&'static str>) -> Self {
        self.dependencies = dependencies;
        self
    }
}

impl Seeder for SqlFileSeeder {
    fn name(&self) -> &'static str {
        self.name
    }

    fn dependencies(&self) -> &[&'static str] {
        &self.dependencies
    }

    fn run<'a>(
        &'a self,
        tx: &'a dyn DatabaseTransaction,
    ) -> BoxFuture<'a, Result<(), PersistenceError>> {
        Box::pin(async move {
            if !self.sql.trim().is_empty() {
                tx.execute(&self.sql).await?;
            }
            Ok(())
        })
    }
}

/// Registry that validates seeder dependencies and executes pending seeders in topological order.
#[derive(Default)]
pub struct SeedRegistry {
    seeders: Vec<Arc<dyn Seeder>>,
}

impl std::fmt::Debug for SeedRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&'static str> = self.seeders.iter().map(|s| s.name()).collect();
        f.debug_struct("SeedRegistry")
            .field("seeders", &names)
            .finish()
    }
}

impl SeedRegistry {
    /// Creates an empty seeder registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a seeder with the registry.
    pub fn register(&mut self, seeder: impl Seeder + 'static) -> &mut Self {
        self.seeders.push(Arc::new(seeder));
        self
    }

    /// Returns the number of registered seeders.
    pub fn len(&self) -> usize {
        self.seeders.len()
    }

    /// Returns `true` if no seeders are registered.
    pub fn is_empty(&self) -> bool {
        self.seeders.is_empty()
    }

    /// Resolves seeder execution order using Kahn's topological sort.
    fn resolve_order(&self) -> Result<Vec<usize>, PersistenceError> {
        let n = self.seeders.len();
        let mut name_to_idx = HashMap::with_capacity(n);
        for (idx, seeder) in self.seeders.iter().enumerate() {
            let name = seeder.name();
            if name_to_idx.insert(name, idx).is_some() {
                return Err(PersistenceError::QueryFailed(format!(
                    "duplicate seeder registered: '{name}'"
                )));
            }
        }

        let mut in_degree = vec![0usize; n];
        let mut adj = vec![Vec::new(); n];

        for (idx, seeder) in self.seeders.iter().enumerate() {
            let mut seen_deps = BTreeSet::new();
            for &dep in seeder.dependencies() {
                if !seen_deps.insert(dep) {
                    continue;
                }
                let &dep_idx = name_to_idx.get(dep).ok_or_else(|| {
                    PersistenceError::QueryFailed(format!(
                        "seeder '{}' depends on missing seeder '{dep}'",
                        seeder.name()
                    ))
                })?;
                adj[dep_idx].push(idx);
                in_degree[idx] += 1;
            }
        }

        let mut queue: VecDeque<usize> = in_degree
            .iter()
            .enumerate()
            .filter_map(|(idx, &deg)| (deg == 0).then_some(idx))
            .collect();

        let mut order = Vec::with_capacity(n);
        while let Some(u) = queue.pop_front() {
            order.push(u);
            for &v in &adj[u] {
                in_degree[v] -= 1;
                if in_degree[v] == 0 {
                    queue.push_back(v);
                }
            }
        }

        if order.len() != n {
            let cycle_nodes: Vec<&'static str> = in_degree
                .iter()
                .enumerate()
                .filter_map(|(idx, &deg)| (deg > 0).then_some(self.seeders[idx].name()))
                .collect();
            return Err(PersistenceError::QueryFailed(format!(
                "dependency cycle detected in seeders: {}",
                cycle_nodes.join(", ")
            )));
        }

        Ok(order)
    }

    /// Runs all registered seeders that have not yet been recorded in `_rume_seeds`.
    pub async fn run_all<B: DatabaseBackend>(
        &self,
        pool: &DatabasePool<B>,
    ) -> Result<SeedReport, PersistenceError> {
        let order = self.resolve_order()?;
        self.execute_order(pool, order).await
    }

    /// Runs only the named seeders (in topological order) that have not yet been recorded.
    pub async fn run_only<B: DatabaseBackend>(
        &self,
        pool: &DatabasePool<B>,
        names: &[&str],
    ) -> Result<SeedReport, PersistenceError> {
        let order = self.resolve_order()?;
        let registered: HashSet<&'static str> = self.seeders.iter().map(|s| s.name()).collect();
        for &requested in names {
            if !registered.contains(requested) {
                return Err(PersistenceError::QueryFailed(format!(
                    "unknown seeder '{requested}'"
                )));
            }
        }

        let selected: HashSet<&str> = names.iter().copied().collect();
        let filtered: Vec<usize> = order
            .into_iter()
            .filter(|&idx| selected.contains(self.seeders[idx].name()))
            .collect();

        self.execute_order(pool, filtered).await
    }

    async fn execute_order<B: DatabaseBackend>(
        &self,
        pool: &DatabasePool<B>,
        order: Vec<usize>,
    ) -> Result<SeedReport, PersistenceError> {
        if order.is_empty() {
            return Ok(SeedReport::default());
        }

        let dialect = pool.backend().dialect();
        pool.execute(create_seeds_table_ddl(dialect)).await?;

        let rows = pool.fetch_all("SELECT name FROM _rume_seeds", &[]).await?;
        let mut applied_set = HashSet::with_capacity(rows.len());
        for row in rows {
            applied_set.insert(row.get::<String>("name")?);
        }

        let mut report = SeedReport::default();
        let insert_sql = insert_seed_query(dialect);

        for idx in order {
            let seeder = Arc::clone(&self.seeders[idx]);
            let name = seeder.name();
            if applied_set.contains(name) {
                report.skipped.push(name.to_string());
                continue;
            }

            let applied_at = current_timestamp_string();
            pool.transaction(move |tx| async move {
                seeder.run(&tx).await?;
                tx.execute_params(
                    insert_sql,
                    &[DbValue::from(name), DbValue::from(applied_at)],
                )
                .await?;
                Ok::<(), PersistenceError>(())
            })
            .await?;

            applied_set.insert(name.to_string());
            report.applied.push(name.to_string());
        }

        Ok(report)
    }
}

fn create_seeds_table_ddl(dialect: &str) -> &'static str {
    match dialect {
        "mysql" => {
            "CREATE TABLE IF NOT EXISTS _rume_seeds (name VARCHAR(255) PRIMARY KEY, applied_at VARCHAR(64) NOT NULL) ENGINE=InnoDB"
        }
        _ => {
            "CREATE TABLE IF NOT EXISTS _rume_seeds (name TEXT PRIMARY KEY, applied_at TEXT NOT NULL)"
        }
    }
}

fn insert_seed_query(dialect: &str) -> &'static str {
    match dialect {
        "postgres" => "INSERT INTO _rume_seeds (name, applied_at) VALUES ($1, $2)",
        _ => "INSERT INTO _rume_seeds (name, applied_at) VALUES (?, ?)",
    }
}

fn current_timestamp_string() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::persistence::{DatabaseConfig, DatabasePool, SqliteBackend};
    use std::sync::{Arc, Mutex};

    struct RecordingSeeder {
        name: &'static str,
        deps: Vec<&'static str>,
        sql: &'static str,
        log: Arc<Mutex<Vec<&'static str>>>,
    }

    impl Seeder for RecordingSeeder {
        fn name(&self) -> &'static str {
            self.name
        }

        fn dependencies(&self) -> &[&'static str] {
            &self.deps
        }

        fn run<'a>(
            &'a self,
            tx: &'a dyn DatabaseTransaction,
        ) -> BoxFuture<'a, Result<(), PersistenceError>> {
            Box::pin(async move {
                self.log.lock().unwrap().push(self.name);
                tx.execute(self.sql).await?;
                Ok(())
            })
        }
    }

    struct FailingSeeder {
        name: &'static str,
    }

    impl Seeder for FailingSeeder {
        fn name(&self) -> &'static str {
            self.name
        }

        fn run<'a>(
            &'a self,
            tx: &'a dyn DatabaseTransaction,
        ) -> BoxFuture<'a, Result<(), PersistenceError>> {
            Box::pin(async move {
                tx.execute("INSERT INTO seed_items (id, label) VALUES (99, 'should_rollback')")
                    .await?;
                Err(PersistenceError::QueryFailed("intentional failure".into()))
            })
        }
    }

    async fn setup_sqlite_pool() -> DatabasePool<SqliteBackend> {
        let cfg = DatabaseConfig::new("sqlite::memory:");
        let backend = SqliteBackend::new(&cfg).await.unwrap();
        let pool = DatabasePool::new(backend, cfg);
        pool.execute("CREATE TABLE seed_items (id INTEGER PRIMARY KEY, label TEXT NOT NULL)")
            .await
            .unwrap();
        pool
    }

    #[tokio::test]
    async fn seed_registry_executes_in_dependency_order_and_is_idempotent() {
        let pool = setup_sqlite_pool().await;
        let log = Arc::new(Mutex::new(Vec::new()));

        let mut registry = SeedRegistry::new();
        // Register child before parent to verify topological ordering.
        registry
            .register(RecordingSeeder {
                name: "posts",
                deps: vec!["users"],
                sql: "INSERT INTO seed_items (id, label) VALUES (2, 'post')",
                log: Arc::clone(&log),
            })
            .register(RecordingSeeder {
                name: "users",
                deps: vec![],
                sql: "INSERT INTO seed_items (id, label) VALUES (1, 'user')",
                log: Arc::clone(&log),
            });

        let first = registry.run_all(&pool).await.unwrap();
        assert_eq!(
            first.applied,
            vec!["users".to_string(), "posts".to_string()]
        );
        assert!(first.skipped.is_empty());
        assert_eq!(*log.lock().unwrap(), vec!["users", "posts"]);

        let second = registry.run_all(&pool).await.unwrap();
        assert!(second.applied.is_empty());
        assert_eq!(
            second.skipped,
            vec!["users".to_string(), "posts".to_string()]
        );
        // Second run must not invoke `run` again.
        assert_eq!(*log.lock().unwrap(), vec!["users", "posts"]);
    }

    #[tokio::test]
    async fn seed_registry_detects_cycles_and_missing_dependencies() {
        let pool = setup_sqlite_pool().await;
        let log = Arc::new(Mutex::new(Vec::new()));

        let mut missing_reg = SeedRegistry::new();
        missing_reg.register(RecordingSeeder {
            name: "posts",
            deps: vec!["nonexistent"],
            sql: "SELECT 1",
            log: Arc::clone(&log),
        });
        let err = missing_reg.run_all(&pool).await.unwrap_err();
        assert!(
            err.to_string().contains("nonexistent"),
            "expected missing dependency error, got: {err}"
        );

        let mut cycle_reg = SeedRegistry::new();
        cycle_reg
            .register(RecordingSeeder {
                name: "a",
                deps: vec!["b"],
                sql: "SELECT 1",
                log: Arc::clone(&log),
            })
            .register(RecordingSeeder {
                name: "b",
                deps: vec!["a"],
                sql: "SELECT 1",
                log: Arc::clone(&log),
            });
        let cycle_err = cycle_reg.run_all(&pool).await.unwrap_err();
        assert!(
            cycle_err.to_string().to_lowercase().contains("cycle"),
            "expected cycle error, got: {cycle_err}"
        );

        // Neither invalid graph should have created `_rume_seeds` or run any seeder.
        assert!(log.lock().unwrap().is_empty());
        let ledger_exists = pool
            .query_scalar(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='_rume_seeds'",
            )
            .await
            .unwrap();
        assert!(ledger_exists.is_none());
    }

    #[tokio::test]
    async fn seed_registry_rolls_back_failed_seeder_without_recording_ledger() {
        let pool = setup_sqlite_pool().await;
        let log = Arc::new(Mutex::new(Vec::new()));

        let mut registry = SeedRegistry::new();
        registry
            .register(RecordingSeeder {
                name: "ok_seed",
                deps: vec![],
                sql: "INSERT INTO seed_items (id, label) VALUES (1, 'ok')",
                log: Arc::clone(&log),
            })
            .register(FailingSeeder { name: "bad_seed" });

        let err = registry.run_all(&pool).await.unwrap_err();
        assert!(err.to_string().contains("intentional failure"));

        // `ok_seed` committed, `bad_seed` rolled back.
        let count = pool
            .query_scalar("SELECT COUNT(*) FROM seed_items")
            .await
            .unwrap();
        assert_eq!(count.as_deref(), Some("1"));

        let ledger_rows = pool
            .fetch_all("SELECT name FROM _rume_seeds", &[])
            .await
            .unwrap();
        let recorded: Vec<String> = ledger_rows
            .iter()
            .map(|r| r.get::<String>("name").unwrap())
            .collect();
        assert_eq!(recorded, vec!["ok_seed".to_string()]);
    }
}
