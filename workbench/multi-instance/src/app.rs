use axum::Router;
use rume::app::init_tracing;
use rume::jobs::postgres::PostgresJobQueue;
use rume::jobs::{JobQueue, JobRegistry, JobWorker};
use rume::outbox::{OutboxMessage, OutboxRelay};
use rume::persistence::config::DatabaseConfig;
use rume::persistence::pool::DatabasePool;
use rume::persistence::postgres::PostgresBackend;
use rume::tenant::model::Tenant;
use rume::tenant::resolver::HeaderTenantResolver;
use std::sync::Arc;
use std::time::Duration;

use crate::config::Config;
use crate::env::Env;
use crate::jobs::{NotificationHandler, SendNotificationJob};
use crate::migrations::workbench_migrator;
use crate::models::IssueCreatedEvent;
use crate::routes::build_api_router;
use crate::state::WorkbenchState;
use rume::events::Event;

/// Builds the complete Axum router with state and middleware for Workbench.
pub fn build_app(state: WorkbenchState, tenant_resolver: HeaderTenantResolver) -> Router {
    build_api_router(state.pool.clone(), tenant_resolver)
        .into_inner()
        .with_state(state)
}

/// Default OS signal handler for SIGINT (Ctrl+C) and SIGTERM.
pub async fn default_shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            tracing::info!("Received SIGINT (Ctrl+C), initiating graceful shutdown");
        },
        _ = terminate => {
            tracing::info!("Received SIGTERM, initiating graceful shutdown");
        },
    }
}

/// Runs the full Rume Workbench application lifecycle, listening for OS signals (SIGINT / SIGTERM).
pub async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    run_with_shutdown(default_shutdown_signal()).await
}

/// Runs the Workbench application lifecycle with a custom shutdown signal future.
pub async fn run_with_shutdown<F>(
    shutdown: F,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    init_tracing();

    let env = rume::env::load::<Env>()?;
    let config = rume::config::load::<Config>()?;

    tracing::info!("Starting {}...", config.app.name);

    // 1. Connect PostgreSQL Database Pool
    let mut pg_config = DatabaseConfig::new(&env.database_url);
    pg_config.max_connections = config.database.max_connections.max(5);
    pg_config.auto_migrate = true;
    let backend = PostgresBackend::connect(&pg_config).await?;
    let pool = DatabasePool::new(backend, pg_config);

    // 2. Bind TCP listener
    let listener = tokio::net::TcpListener::bind((env.host, env.port)).await?;
    tracing::info!("Workbench listening on {}:{}", env.host, env.port);

    run_with_listener_and_shutdown(listener, pool, shutdown).await
}

/// Runs the Workbench application lifecycle with an explicit TCP listener, pool, and shutdown signal future.
pub async fn run_with_listener_and_shutdown<F>(
    listener: tokio::net::TcpListener,
    pool: DatabasePool<PostgresBackend>,
    shutdown: F,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    // 1. Execute Pending Migrations
    let migrator = workbench_migrator();
    pool.apply_auto_migrations(&migrator).await?;

    // 2. Initialize Postgres Job Queue & Registry
    let job_queue = Arc::new(PostgresJobQueue::new(pool.backend().pool().clone()));
    let notification_handler = NotificationHandler::new();
    let mut registry = JobRegistry::new();
    registry.register(notification_handler.clone());

    // 3. Start Background Job Worker
    let worker = JobWorker::new(Arc::clone(&job_queue), Arc::new(registry));
    let worker_handle = worker.start();

    // 4. Initialize & Start Outbox Relay
    // Dispatches IssueCreated events to SendNotificationJob
    let queue_for_relay = Arc::clone(&job_queue);
    let relay = OutboxRelay::new(Arc::new(pool.clone()), move |msg: &OutboxMessage| {
        let queue = Arc::clone(&queue_for_relay);
        let msg = msg.clone();
        async move {
            if msg.event_type == IssueCreatedEvent::TYPE {
                let event: IssueCreatedEvent = serde_json::from_slice(&msg.payload)
                    .map_err(|e| rume::outbox::OutboxError::Repository(e.to_string()))?;
                let job = SendNotificationJob {
                    tenant_id: event.tenant_id.clone(),
                    recipient: event
                        .assignee_id
                        .unwrap_or_else(|| "unassigned@example.com".to_string()),
                    subject: format!("New Issue Created: {}", event.title),
                    body: format!(
                        "Issue {} created in project {}",
                        event.issue_id, event.project_id
                    ),
                    idempotency_key: format!("notify-issue-{}", event.issue_id),
                };
                queue
                    .enqueue(job)
                    .await
                    .map_err(|e| rume::outbox::OutboxError::Dispatch(e.to_string()))?;
            }
            Ok(())
        }
    });
    let relay_handle = relay.start(Duration::from_millis(250));

    // 5. Setup Multi-Tenant Resolver
    let tenant_resolver = HeaderTenantResolver::new()
        .add_tenant(Tenant::new("acme-corp", "acme-corp"))
        .add_tenant(Tenant::new("globex", "globex"));

    // 6. Assemble State & Router
    let state = WorkbenchState {
        pool: pool.clone(),
        job_queue,
        notification_handler,
        relay: Some(relay.clone()),
        worker: Some(worker.clone()),
    };
    let app = build_app(state, tenant_resolver);

    // 7. Bind & Serve HTTP Server with Graceful Shutdown
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;

    // 8. Supervised Graceful Shutdown Drain Sequence
    tracing::info!("Shutting down Workbench background workers, relay, and connection pool...");
    let drain_timeout = Duration::from_secs(5);

    let worker_ok = worker_handle.shutdown(drain_timeout).await;
    let relay_drain_ok = relay.drain(drain_timeout).await;
    let relay_join_ok = match tokio::time::timeout(drain_timeout, relay_handle).await {
        Ok(Ok(())) => true,
        Ok(Err(e)) => {
            tracing::error!("Relay background task join error: {e}");
            false
        }
        Err(_) => {
            tracing::error!("Relay background task shutdown timed out");
            false
        }
    };
    let pool_ok = pool.close_and_drain(drain_timeout).await;

    if !worker_ok || !relay_drain_ok || !relay_join_ok || !pool_ok {
        return Err(format!(
            "Workbench graceful shutdown failure: worker_ok={worker_ok}, relay_drain_ok={relay_drain_ok}, relay_join_ok={relay_join_ok}, pool_ok={pool_ok}"
        ).into());
    }

    tracing::info!("Workbench stopped cleanly.");
    Ok(())
}
