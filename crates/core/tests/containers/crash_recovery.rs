//! Subprocess crash resilience and recovery integration test suite.
//!
//! Executes real child processes against PostgreSQL and Redis, abruptly terminates them
//! using OS-level process kills (`child.kill()`), and verifies that subsequent processes
//! cleanly recover abandoned job leases, expired outbox claims, and coordination locks.

#![cfg(all(feature = "postgres", feature = "redis"))]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::Duration;

use rume::jobs::{Job, JobQueue, PostgresJobQueue, RetryPolicy};
use serde::{Deserialize, Serialize};

use crate::common;

#[derive(Debug, Serialize, Deserialize)]
struct CrashTestJob {
    job_id: String,
    value: i64,
}

impl Job for CrashTestJob {
    const NAME: &'static str = "CrashTestJob";

    fn retry_policy(&self) -> RetryPolicy {
        RetryPolicy {
            max_attempts: 3,
            initial_backoff_ms: 100,
            max_backoff_ms: 500,
            backoff_factor: 2.0,
        }
    }
}

fn crash_harness_binary() -> std::path::PathBuf {
    // Cargo automatically provides this environment variable when testing binary targets.
    // If not present, fallback to searching target/debug/rume-crash-harness.
    if let Ok(bin) = std::env::var("CARGO_BIN_EXE_rume-crash-harness") {
        std::path::PathBuf::from(bin)
    } else {
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let bin = manifest_dir.join("../target/debug/rume-crash-harness");
        if bin.exists() {
            bin
        } else {
            manifest_dir.join("target/debug/rume-crash-harness")
        }
    }
}

#[tokio::test]
async fn test_process_crash_during_worker_execution_and_recovery() {
    let _pg = common::containers::shared_postgres().await;
    let (_db, config) = common::containers::create_isolated_postgres_db().await;

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&config.url)
        .await
        .expect("connect to pg");

    let queue = PostgresJobQueue::new(pool.clone());
    queue.init_schema().await.expect("schema initialized");

    // 1. Enqueue durable job
    let job_id = queue
        .enqueue(CrashTestJob {
            job_id: "tx_subproc_1".into(),
            value: 42,
        })
        .await
        .expect("enqueue job");

    let bin = crash_harness_binary();

    // 2. Spawn Child Process A to claim the job and hold the lease
    let mut child_a = Command::new(&bin)
        .arg("worker")
        .arg("claim-and-hold")
        .arg("--db-url")
        .arg(&config.url)
        .arg("--lease-ms")
        .arg("400")
        .arg("--worker-id")
        .arg("worker-process-a")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn process a");

    let stdout_a = child_a.stdout.take().expect("capture child a stdout");
    let mut reader_a = BufReader::new(stdout_a);
    let mut line = String::new();

    // Wait until Process A claims the job and is actively holding the lease
    reader_a
        .read_line(&mut line)
        .expect("read line from child a");
    assert!(
        line.starts_with("CLAIM_ACQUIRED:"),
        "expected CLAIM_ACQUIRED, got: {line}"
    );

    // 3. Abruptly kill Process A (simulating SIGKILL / crash)
    child_a.kill().expect("kill process a");
    let _ = child_a.wait();

    // 4. Wait for the 400ms lease to expire in PostgreSQL
    tokio::time::sleep(Duration::from_millis(500)).await;

    // 5. Spawn Child Process B to recover and complete the job
    let output_b = Command::new(&bin)
        .arg("worker")
        .arg("recover-worker")
        .arg("--db-url")
        .arg(&config.url)
        .arg("--worker-id")
        .arg("worker-process-b")
        .output()
        .expect("execute process b");

    assert!(
        output_b.status.success(),
        "process b should exit successfully: {:?}",
        String::from_utf8_lossy(&output_b.stderr)
    );
    let stdout_b = String::from_utf8_lossy(&output_b.stdout);
    assert!(
        stdout_b.contains(&format!("JOB_ACKED:{job_id}")),
        "process b must ack recovered job: {stdout_b}"
    );

    // 6. Verify in database that job is marked completed
    let row: (String,) = sqlx::query_as("SELECT status FROM rume_jobs WHERE id = $1")
        .bind(&job_id)
        .fetch_one(&pool)
        .await
        .expect("fetch job status");

    assert_eq!(row.0, "completed", "job must be completed after recovery");
}

#[tokio::test]
async fn test_process_crash_after_business_tx_and_outbox_recovery() {
    let _pg = common::containers::shared_postgres().await;
    let (_db, config) = common::containers::create_isolated_postgres_db().await;

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&config.url)
        .await
        .expect("connect to pg");

    // Initialize core schema
    let backend = rume::persistence::postgres::PostgresBackend::from_pool(pool.clone());
    let migrator = rume::persistence::migration::Migrator::new().with_rume_core("postgres");
    migrator
        .run_pending(&backend)
        .await
        .expect("core migrations");

    let bin = crash_harness_binary();

    // 1. Process A commits business transaction + outbox message, then crashes with exit(137)
    let output_a = Command::new(&bin)
        .arg("outbox")
        .arg("tx-commit-and-crash")
        .arg("--db-url")
        .arg(&config.url)
        .arg("--lease-secs")
        .arg("1")
        .output()
        .expect("run process a");

    // Exit code 137 verifies the process exited abruptly
    assert_eq!(
        output_a.status.code(),
        Some(137),
        "process a must exit with code 137 (simulated crash)"
    );

    // Verify business state was committed despite crash
    let order_count: (i64,) = sqlx::query_as("SELECT count(*) FROM crash_test_orders")
        .fetch_one(&pool)
        .await
        .expect("query crash_test_orders");
    assert_eq!(order_count.0, 1, "business transaction must be committed");

    // 2. Process A2 claims the outbox message, then gets killed via SIGKILL
    let mut child_a2 = Command::new(&bin)
        .arg("outbox")
        .arg("claim-and-hold")
        .arg("--db-url")
        .arg(&config.url)
        .arg("--lease-secs")
        .arg("1")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn process a2");

    let stdout_a2 = child_a2.stdout.take().expect("capture child a2 stdout");
    let mut reader_a2 = BufReader::new(stdout_a2);
    let mut line = String::new();
    reader_a2
        .read_line(&mut line)
        .expect("read line from child a2");
    assert!(
        line.starts_with("OUTBOX_CLAIMED:"),
        "expected OUTBOX_CLAIMED, got: {line}"
    );

    // Terminate Process A2 abruptly
    child_a2.kill().expect("kill process a2");
    let _ = child_a2.wait();

    // 3. Wait for 1-second outbox lease to expire
    tokio::time::sleep(Duration::from_millis(1100)).await;

    // 4. Process B recovers expired outbox claim and dispatches it
    let output_b = Command::new(&bin)
        .arg("outbox")
        .arg("relay-recover")
        .arg("--db-url")
        .arg(&config.url)
        .arg("--lease-secs")
        .arg("1")
        .output()
        .expect("run process b");

    assert!(
        output_b.status.success(),
        "process b must exit 0: {:?}",
        String::from_utf8_lossy(&output_b.stderr)
    );
    let stdout_b = String::from_utf8_lossy(&output_b.stdout);
    assert!(
        stdout_b.contains("OUTBOX_RECOVERED:published=1"),
        "relay must publish recovered message: {stdout_b}"
    );

    // 5. Verify outbox table reflects published status
    let outbox_status: (String,) = sqlx::query_as("SELECT status FROM rume_outbox LIMIT 1")
        .fetch_one(&pool)
        .await
        .expect("query outbox status");
    assert_eq!(
        outbox_status.0, "published",
        "outbox message must be published"
    );
}

#[tokio::test]
async fn test_process_crash_distributed_leader_takeover() {
    use rume::cache::redis::RedisCacheBackend;
    use rume::coordination::{LeaderElector, RedisLeaderElector};

    let _redis = common::containers::shared_redis().await;
    let redis_url = common::containers::shared_redis_url().await;

    let bin = crash_harness_binary();
    let prefix = format!("test:crash:leader:{}:", uuid::Uuid::new_v4().simple());
    let role = "worker-coordinator";

    // Initialize test-side Rume elector to observe and verify coordination state
    let backend = RedisCacheBackend::new(&redis_url).expect("connect redis");
    let test_elector = RedisLeaderElector::new(backend.pool().clone()).with_prefix(prefix.clone());

    // 1. Process A acquires leader lease through Rume with 600ms TTL
    let mut child_a = Command::new(&bin)
        .arg("leader")
        .arg("acquire-and-hold")
        .arg("--redis-url")
        .arg(&redis_url)
        .arg("--prefix")
        .arg(&prefix)
        .arg("--role")
        .arg(role)
        .arg("--ttl-ms")
        .arg("600")
        .arg("--identity")
        .arg("leader-node-a")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn child a");

    let stdout_a = child_a.stdout.take().expect("capture child a stdout");
    let mut reader_a = BufReader::new(stdout_a);
    let mut line = String::new();
    reader_a
        .read_line(&mut line)
        .expect("read line from child a");
    assert!(
        line.starts_with("LEADER_ACQUIRED:leader-node-a:"),
        "child a must output LEADER_ACQUIRED with term: {line}"
    );

    // Parse election term obtained by Process A
    let term_a: u64 = line
        .trim()
        .strip_prefix("LEADER_ACQUIRED:leader-node-a:")
        .expect("extract term_a")
        .parse()
        .expect("valid u64 term");
    assert!(term_a >= 1, "term must be positive integer");

    // Verify Process A is active leader via Rume framework elector
    let initial_leader = test_elector
        .get_leader(role)
        .await
        .expect("get_leader")
        .expect("leader must exist");
    assert_eq!(initial_leader.candidate_id, "leader-node-a");
    assert_eq!(initial_leader.term, term_a);

    // 2. Kill Process A abruptly via OS SIGKILL
    child_a.kill().expect("kill child a");
    let _ = child_a.wait();

    // 3. Wait for TTL (600ms) to expire
    tokio::time::sleep(Duration::from_millis(700)).await;

    // 4. Process B takes over leadership through Rume
    let output_b = Command::new(&bin)
        .arg("leader")
        .arg("takeover")
        .arg("--redis-url")
        .arg(&redis_url)
        .arg("--prefix")
        .arg(&prefix)
        .arg("--role")
        .arg(role)
        .arg("--ttl-ms")
        .arg("600")
        .arg("--identity")
        .arg("leader-node-b")
        .output()
        .expect("run process b");

    assert!(
        output_b.status.success(),
        "process b must succeed in takeover: {:?}",
        String::from_utf8_lossy(&output_b.stderr)
    );
    let stdout_b = String::from_utf8_lossy(&output_b.stdout);
    assert!(
        stdout_b.contains("TAKEOVER_SUCCESS:leader-node-b:"),
        "process b must output TAKEOVER_SUCCESS with term: {stdout_b}"
    );

    // Parse election term obtained by Process B
    let term_b: u64 = stdout_b
        .lines()
        .find(|l| l.starts_with("TAKEOVER_SUCCESS:leader-node-b:"))
        .expect("find takeover line")
        .strip_prefix("TAKEOVER_SUCCESS:leader-node-b:")
        .expect("extract term_b")
        .parse()
        .expect("valid u64 term");

    // 5. Assert strictly monotonic election term increment across process crashes
    assert!(
        term_b > term_a,
        "new leader must receive strictly greater election term: term_b={term_b} > term_a={term_a}"
    );

    // 6. Verify through Rume framework that Process B is the confirmed leader
    let current_leader = test_elector
        .get_leader(role)
        .await
        .expect("get_leader after takeover")
        .expect("leader must exist");
    assert_eq!(current_leader.candidate_id, "leader-node-b");
    assert_eq!(current_leader.term, term_b);

    // 7. Verify stale-owner behavior: a resurrection of Process A cannot renew leadership
    let stale_renewal = test_elector
        .renew(role, "leader-node-a", Duration::from_millis(600))
        .await
        .expect("renewal query succeeds");
    assert!(
        stale_renewal.is_none(),
        "stale owner Process A must be rejected after Process B took over with term {term_b}"
    );
}

#[tokio::test]
async fn test_raw_redis_ttl_lock_recovery() {
    let _redis = common::containers::shared_redis().await;
    let redis_url = common::containers::shared_redis_url().await;
    let client = redis::Client::open(redis_url).expect("redis client");
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .expect("connection");

    let key = format!("test:crash:raw_lock:{}", uuid::Uuid::new_v4());

    // Acquire lock with 300ms TTL
    let acquired: bool = redis::cmd("SET")
        .arg(&key)
        .arg("node-1")
        .arg("NX")
        .arg("PX")
        .arg(300)
        .query_async(&mut conn)
        .await
        .unwrap_or(false);
    assert!(acquired, "initial lock acquisition succeeds");

    // Another node fails immediately
    let failed: bool = redis::cmd("SET")
        .arg(&key)
        .arg("node-2")
        .arg("NX")
        .arg("PX")
        .arg(300)
        .query_async(&mut conn)
        .await
        .unwrap_or(false);
    assert!(!failed, "competing lock acquisition is rejected");

    // Wait for TTL expiration
    tokio::time::sleep(Duration::from_millis(400)).await;

    // Node 2 successfully acquires after node 1 simulated crash / TTL expiration
    let recovered: bool = redis::cmd("SET")
        .arg(&key)
        .arg("node-2")
        .arg("NX")
        .arg("PX")
        .arg(300)
        .query_async(&mut conn)
        .await
        .unwrap_or(false);
    assert!(recovered, "lock recovery after TTL expiration succeeds");
}
