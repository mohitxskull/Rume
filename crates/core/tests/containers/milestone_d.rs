//! Live container integration tests for Milestone D backends.
//!
//! Covers `MysqlJobQueue`, `PostgresPubSub`, `RedisPubSub`, `SqlSagaStore` (Postgres/MySQL),
//! and `RedisSagaStore`. Uses `crates/core/tests/common/` isolated container helpers.

#[cfg(feature = "mysql")]
mod mysql_tests {
    use rume::jobs::{Job, JobQueue, MysqlJobQueue};
    use rume::saga::{Saga, SagaError, SagaStatus, SagaStore, SqlSagaStore};
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;

    use crate::common;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct MysqlSmokeJob {
        label: String,
    }

    impl Job for MysqlSmokeJob {
        const NAME: &'static str = "milestone_d_mysql_smoke";
    }

    #[tokio::test]
    async fn mysql_job_queue_and_sql_saga_store_live() {
        let (_db_name, config) = common::containers::create_isolated_mysql_db().await;
        let pool = sqlx::mysql::MySqlPoolOptions::new()
            .max_connections(5)
            .connect(&config.url)
            .await
            .expect("connect mysql");

        let queue = MysqlJobQueue::new(pool.clone());
        queue.init_schema().await.expect("init mysql jobs schema");
        queue
            .enqueue(MysqlSmokeJob {
                label: "hello".into(),
            })
            .await
            .expect("enqueue mysql job");
        let job = queue
            .dequeue_for("mysql-worker-1")
            .await
            .expect("dequeue mysql job")
            .expect("job present");
        queue
            .ack(&job.id, job.lease_token.as_deref().unwrap_or_default())
            .await
            .expect("ack mysql job");

        let saga_store = Arc::new(SqlSagaStore::mysql(pool));
        saga_store
            .init_schema()
            .await
            .expect("init mysql saga schema");

        let saga = Saga::<()>::new("mysql-saga")
            .with_store(Arc::clone(&saga_store) as Arc<dyn SagaStore>)
            .step("step-1", |_| async { Ok(()) }, |_| async { Ok(()) })
            .step(
                "step-2",
                |_| async {
                    Err(SagaError::ActionFailed {
                        step: "step-2".into(),
                        message: "boom".into(),
                    })
                },
                |_| async { Ok(()) },
            );

        let report = saga
            .run_with_id("mysql-saga-1", ())
            .await
            .expect("run mysql saga");
        assert!(matches!(report.status, SagaStatus::Compensated { .. }));
        let loaded = saga_store
            .load_record("mysql-saga-1")
            .await
            .expect("load mysql saga")
            .expect("record exists");
        assert!(matches!(loaded.status, SagaStatus::Compensated { .. }));
    }
}

#[cfg(feature = "postgres")]
mod postgres_tests {
    use rume::pubsub::{PostgresPubSub, PubSub};
    use rume::saga::{Saga, SagaStatus, SagaStore, SqlSagaStore};
    use std::sync::Arc;
    use std::time::Duration;

    use crate::common;

    #[tokio::test]
    async fn postgres_pubsub_and_sql_saga_store_live() {
        let (_db_name, config) = common::containers::create_isolated_postgres_db().await;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&config.url)
            .await
            .expect("connect postgres");

        let pubsub = PostgresPubSub::new(pool.clone());
        let mut sub = pubsub
            .subscribe("milestone_d_pg_topic")
            .await
            .expect("pg subscribe");
        pubsub
            .publish("milestone_d_pg_topic", b"pg-hello")
            .await
            .expect("pg publish");

        let msg = tokio::time::timeout(Duration::from_secs(3), sub.recv())
            .await
            .expect("pg pubsub timeout")
            .expect("pg recv ok")
            .expect("pg message present");
        assert_eq!(msg.payload, b"pg-hello");

        let saga_store = Arc::new(SqlSagaStore::postgres(pool));
        saga_store.init_schema().await.expect("init pg saga schema");

        let saga = Saga::<()>::new("pg-saga")
            .with_store(Arc::clone(&saga_store) as Arc<dyn SagaStore>)
            .step("step-1", |_| async { Ok(()) }, |_| async { Ok(()) });
        let report = saga
            .run_with_id("pg-saga-1", ())
            .await
            .expect("run pg saga");
        assert_eq!(report.status, SagaStatus::Success);
    }
}

#[cfg(feature = "redis")]
mod redis_tests {
    use rume::pubsub::{PubSub, RedisPubSub};
    use rume::saga::{RedisSagaStore, Saga, SagaStatus, SagaStore};
    use std::sync::Arc;
    use std::time::Duration;

    use crate::common;

    #[tokio::test]
    async fn redis_pubsub_and_redis_saga_store_live() {
        let url = common::containers::shared_redis_url().await;
        let pubsub = RedisPubSub::from_url(&url).expect("redis pubsub");
        let topic = format!("milestone_d_redis_{}", uuid::Uuid::new_v4().simple());
        let mut sub = pubsub.subscribe(&topic).await.expect("redis subscribe");
        pubsub
            .publish(&topic, b"redis-hello")
            .await
            .expect("redis publish");

        let msg = tokio::time::timeout(Duration::from_secs(3), sub.recv())
            .await
            .expect("redis pubsub timeout")
            .expect("redis recv ok")
            .expect("redis message present");
        assert_eq!(msg.payload, b"redis-hello");

        let pool = common::containers::create_isolated_redis_pool().await;
        let saga_store = Arc::new(RedisSagaStore::new(pool));
        let saga_id = format!("redis-saga-{}", uuid::Uuid::new_v4().simple());
        let saga = Saga::<()>::new("redis-saga")
            .with_store(Arc::clone(&saga_store) as Arc<dyn SagaStore>)
            .step("step-1", |_| async { Ok(()) }, |_| async { Ok(()) });
        let report = saga
            .run_with_id(&saga_id, ())
            .await
            .expect("run redis saga");
        assert_eq!(report.status, SagaStatus::Success);
        let loaded = saga_store
            .load_record(&saga_id)
            .await
            .expect("load redis saga")
            .expect("record exists");
        assert_eq!(loaded.status, SagaStatus::Success);
    }
}
