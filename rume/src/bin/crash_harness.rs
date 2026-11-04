//! Subprocess crash and recovery harness for production failure qualification.
//!
//! Spawns dedicated child processes that perform transactional persistence, durable outbox
//! claims, or worker job leases, and then either hang (for external SIGKILL) or abruptly exit
//! (`exit(137)`) without executing drop handlers, in-memory cleanup, or graceful drain.

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use rume::jobs::{JobQueue, PostgresJobQueue};
use rume::outbox::{OutboxMessage, OutboxRelay, OutboxRelayConfig, OutboxRepository, TransactionalOutbox};
use rume::persistence::config::DatabaseConfig;
use rume::persistence::pool::DatabasePool;
use rume::persistence::postgres::PostgresBackend;
use rume::persistence::DatabaseTransaction;
use sqlx::postgres::PgPoolOptions;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: rume-crash-harness <worker|outbox|leader> <action> [options...]");
        std::process::exit(1);
    }

    let mode = &args[1];
    let action = &args[2];

    match mode.as_str() {
        "worker" => handle_worker(action, &args[3..]).await?,
        "outbox" => handle_outbox(action, &args[3..]).await?,
        "leader" => handle_leader(action, &args[3..]).await?,
        other => {
            eprintln!("Unknown mode: {other}");
            std::process::exit(1);
        }
    }

    Ok(())
}

async fn handle_worker(action: &str, args: &[String]) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let db_url = get_arg(args, "--db-url").expect("--db-url required");
    let pool = PgPoolOptions::new().max_connections(5).connect(&db_url).await?;
    let lease_ms: u64 = get_arg(args, "--lease-ms").unwrap_or_else(|| "1000".into()).parse()?;
    let queue = PostgresJobQueue::new(pool.clone()).with_lease_duration(Duration::from_millis(lease_ms));
    queue.init_schema().await?;

    match action {
        "claim-and-hold" => {
            let _worker_id = get_arg(args, "--worker-id").unwrap_or_else(|| "worker-process-a".into());
            let job = queue.dequeue().await?;
            if let Some(j) = job {
                println!("CLAIM_ACQUIRED:{}", j.id);
                std::io::stdout().flush()?;
                // Hold lease until killed by parent process (SIGKILL)
                tokio::time::sleep(Duration::from_secs(300)).await;
            } else {
                eprintln!("No pending job found to claim");
                std::process::exit(2);
            }
        }
        "execute-and-crash-before-ack" => {
            let worker_id = get_arg(args, "--worker-id").unwrap_or_else(|| "worker-process-a".into());
            let job = queue.dequeue().await?;
            if let Some(j) = job {
                // Perform side-effect: insert into test audit marker table
                sqlx::query("CREATE TABLE IF NOT EXISTS crash_test_side_effects (job_id TEXT PRIMARY KEY, executed_by TEXT, executed_at BIGINT)")
                    .execute(&pool)
                    .await?;
                sqlx::query("INSERT INTO crash_test_side_effects (job_id, executed_by, executed_at) VALUES ($1, $2, (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT)")
                    .bind(&j.id)
                    .bind(&worker_id)
                    .execute(&pool)
                    .await?;

                println!("SIDE_EFFECT_EXECUTED:{}", j.id);
                std::io::stdout().flush()?;
                // Abrupt crash without running ack or drop cleanup
                std::process::exit(137);
            } else {
                eprintln!("No pending job to execute");
                std::process::exit(2);
            }
        }
        "recover-worker" => {
            let _worker_id = get_arg(args, "--worker-id").unwrap_or_else(|| "worker-process-b".into());
            // Attempt to claim the expired job
            let job = queue.dequeue().await?;
            if let Some(j) = job {
                let lease_token = j.lease_token.clone().expect("claimed job must have lease token");
                queue.ack(&j.id, &lease_token).await?;
                println!("JOB_ACKED:{}", j.id);
                std::io::stdout().flush()?;
            } else {
                eprintln!("Recovery worker found no expired/pending jobs");
                std::process::exit(3);
            }
        }
        other => {
            eprintln!("Unknown worker action: {other}");
            std::process::exit(1);
        }
    }
    Ok(())
}

async fn handle_outbox(action: &str, args: &[String]) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let db_url = get_arg(args, "--db-url").expect("--db-url required");
    let config = DatabaseConfig::new(&db_url).with_max_connections(5);
    let backend = PostgresBackend::connect(&config).await?;
    let pool = Arc::new(DatabasePool::new(backend, config));

    let lease_secs: u64 = get_arg(args, "--lease-secs").unwrap_or_else(|| "1".into()).parse()?;

    match action {
        "tx-commit-and-crash" => {
            let payload = get_arg(args, "--payload").unwrap_or_else(|| "{\"event\":\"order_created\"}".into());
            let outbox_id = uuid::Uuid::new_v4().to_string();
            let saved_id = outbox_id.clone();
            let pool_ref = Arc::clone(&pool);

            let msg_id = outbox_id.clone();

            pool.transaction(|tx| async move {
                // Business state table
                tx.execute("CREATE TABLE IF NOT EXISTS crash_test_orders (id TEXT PRIMARY KEY, amount INT)")
                    .await?;
                tx.execute(&format!("INSERT INTO crash_test_orders (id, amount) VALUES ('{}', 100)", saved_id))
                    .await?;
                // Enlist outbox message in the same atomic transaction
                let outbox_msg = OutboxMessage::new(msg_id, "OrderCreated", payload.into_bytes());
                pool_ref
                    .save_tx(&tx, outbox_msg)
                    .await
                    .map_err(|e| rume::persistence::PersistenceError::TransactionError(e.to_string()))?;
                Ok::<_, rume::persistence::PersistenceError>(())
            })
            .await?;

            println!("TX_COMMITTED:{}", outbox_id);
            std::io::stdout().flush()?;
            // Crash immediately after committing transaction before any relay can process
            std::process::exit(137);
        }
        "claim-and-hold" => {
            let batch = pool.claim_pending_with_lease(1, lease_secs).await?;
            if let Some(msg) = batch.first() {
                println!("OUTBOX_CLAIMED:{}", msg.id);
                std::io::stdout().flush()?;
                // Hold lease until killed by parent (SIGKILL)
                tokio::time::sleep(Duration::from_secs(300)).await;
            } else {
                eprintln!("No pending outbox message to claim");
                std::process::exit(2);
            }
        }
        "relay-recover" => {
            let relay_config = OutboxRelayConfig {
                lease_duration_secs: lease_secs,
                batch_size: 10,
                poll_interval: Duration::from_millis(50),
                max_retries: 3,
            };
            let relay = OutboxRelay::new(Arc::clone(&pool), |_msg: &OutboxMessage| async { Ok(()) })
                .with_config(relay_config);
            let poll_result = relay.poll_and_dispatch(10).await?;

            println!("OUTBOX_RECOVERED:published={}", poll_result.published);
            std::io::stdout().flush()?;
        }
        other => {
            eprintln!("Unknown outbox action: {other}");
            std::process::exit(1);
        }
    }
    Ok(())
}

async fn handle_leader(action: &str, args: &[String]) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let redis_url = get_arg(args, "--redis-url").expect("--redis-url required");
    let client = redis::Client::open(redis_url.clone())?;
    let mut conn = client.get_multiplexed_async_connection().await?;
    let key = get_arg(args, "--key").unwrap_or_else(|| "test:crash:leader".into());
    let identity = get_arg(args, "--identity").unwrap_or_else(|| "leader-process-a".into());
    let ttl_ms: u64 = get_arg(args, "--ttl-ms").unwrap_or_else(|| "500".into()).parse()?;

    match action {
        "acquire-and-hold" => {
            let acquired: bool = redis::cmd("SET")
                .arg(&key)
                .arg(&identity)
                .arg("NX")
                .arg("PX")
                .arg(ttl_ms)
                .query_async(&mut conn)
                .await
                .unwrap_or(false);

            if acquired {
                println!("LEADER_ACQUIRED:{}", identity);
                std::io::stdout().flush()?;
                tokio::time::sleep(Duration::from_secs(300)).await;
            } else {
                eprintln!("Failed to acquire leader lock");
                std::process::exit(2);
            }
        }
        "takeover" => {
            let acquired: bool = redis::cmd("SET")
                .arg(&key)
                .arg(&identity)
                .arg("NX")
                .arg("PX")
                .arg(ttl_ms)
                .query_async(&mut conn)
                .await
                .unwrap_or(false);

            if acquired {
                println!("TAKEOVER_SUCCESS:{}", identity);
                std::io::stdout().flush()?;
            } else {
                eprintln!("Takeover failed; lock still held");
                std::process::exit(3);
            }
        }
        other => {
            eprintln!("Unknown leader action: {other}");
            std::process::exit(1);
        }
    }
    Ok(())
}

fn get_arg(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|idx| args.get(idx + 1).cloned())
}
