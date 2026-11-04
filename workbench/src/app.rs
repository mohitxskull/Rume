use std::sync::Arc;
use std::time::Duration;
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
    build_api_router(tenant_resolver).with_state(state)
}

/// Runs the full Rume Workbench application lifecycle.
pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
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

    // 2. Execute Pending Migrations
    let migrator = workbench_migrator();
    pool.apply_auto_migrations(&migrator).await?;

    // 3. Initialize Postgres Job Queue & Registry
    let job_queue = Arc::new(PostgresJobQueue::new(pool.backend().pool().clone()));
    let notification_handler = NotificationHandler::new();
    let mut registry = JobRegistry::new();
    registry.register(notification_handler.clone());

    // 4. Start Background Job Worker
    let worker = JobWorker::new(Arc::clone(&job_queue), Arc::new(registry));
    let _worker_handle = worker.start();

    // 5. Initialize & Start Outbox Relay
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
    let _relay_handle = relay.start(Duration::from_millis(250));

    // 6. Setup Multi-Tenant Resolver
    let tenant_resolver = HeaderTenantResolver::new()
        .add_tenant(Tenant::new("acme-corp", "acme-corp"))
        .add_tenant(Tenant::new("globex", "globex"));

    // 7. Assemble State & Router
    let state = WorkbenchState {
        pool: pool.clone(),
        job_queue,
        notification_handler,
        relay: Some(relay.clone()),
        worker: Some(worker.clone()),
    };
    let app = build_app(state, tenant_resolver);

    // 8. Bind & Serve HTTP Server
    let listener = tokio::net::TcpListener::bind((env.host, env.port)).await?;
    tracing::info!("Workbench listening on {}:{}", env.host, env.port);
    axum::serve(listener, app).await?;

    // 9. Graceful Shutdown Drain
    tracing::info!("Shutting down Workbench...");
    worker.drain(Duration::from_secs(5)).await;
    relay.drain(Duration::from_secs(5)).await;
    pool.close_and_drain(Duration::from_secs(5)).await;

    Ok(())
}
