use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use serde_json::json;

struct ComposeGuard {
    compose_file: PathBuf,
    disarmed: Arc<AtomicBool>,
}

impl ComposeGuard {
    fn new(compose_file: PathBuf) -> Self {
        Self {
            compose_file,
            disarmed: Arc::new(AtomicBool::new(false)),
        }
    }

    fn run_cmd(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = Command::new("docker");
        cmd.arg("compose");
        cmd.arg("-f");
        cmd.arg(&self.compose_file);
        for a in args {
            cmd.arg(a);
        }
        cmd.output().expect("failed to execute docker compose")
    }

    fn stop_and_down(&self) {
        let _ = self.run_cmd(&["down", "-v", "--remove-orphans"]);
    }
}

impl Drop for ComposeGuard {
    fn drop(&mut self) {
        if !self.disarmed.load(Ordering::SeqCst) {
            eprintln!("[compose_system_test] Tearing down docker compose resources...");
            self.stop_and_down();
        }
    }
}

fn is_docker_available() -> bool {
    Command::new("docker")
        .args(["info"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

async fn wait_for_health(url: &str, timeout: Duration) -> Result<(), String> {
    let client = reqwest::Client::new();
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if let Ok(resp) = client.get(url).send().await
            && resp.status().is_success()
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(format!("Timed out waiting for {url} to become healthy"))
}

#[tokio::test]
async fn test_compose_multi_instance_system_qualification() {
    if !is_docker_available() {
        eprintln!("[compose_system_test] Docker daemon is not available; skipping compose test.");
        return;
    }

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let compose_file = manifest_dir.join("docker-compose.yml");
    assert!(
        compose_file.exists(),
        "docker-compose.yml must exist at {}",
        compose_file.display()
    );

    let guard = ComposeGuard::new(compose_file.clone());

    // Clean any residual state
    guard.stop_and_down();

    // 0. Ensure target/release/workbench exists and build image
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("cannot determine workspace root");
    let binary_path = workspace_root.join("target").join("release").join("workbench");

    if !binary_path.exists() {
        println!("[compose_system_test] Building target/release/workbench binary...");
        let build_status = Command::new("cargo")
            .current_dir(workspace_root)
            .args(["build", "--release", "-p", "workbench", "--bin", "workbench"])
            .status()
            .expect("failed to invoke cargo build --release");
        assert!(build_status.success(), "cargo build --release failed");
    }

    println!("[compose_system_test] Building Docker Compose images...");
    let build_out = guard.run_cmd(&["build"]);
    assert!(
        build_out.status.success(),
        "docker compose build failed: {}",
        String::from_utf8_lossy(&build_out.stderr)
    );

    // =========================================================================
    // Scenario 1 & 8: Clean Deployment & Concurrent Startup
    // Application starts against healthy dependencies and applies migrations
    // concurrently without corrupting or racing migration execution.
    // =========================================================================
    println!("[compose_system_test] Scenario 1 & 8: Concurrent Startup & Clean Deployment...");
    let up_out = guard.run_cmd(&["up", "-d"]);
    assert!(
        up_out.status.success(),
        "docker compose up failed: {}",
        String::from_utf8_lossy(&up_out.stderr)
    );

    // Wait for both instances to be healthy
    let timeout = Duration::from_secs(35);
    wait_for_health("http://127.0.0.1:8081/health", timeout)
        .await
        .expect("Instance 1 failed to reach healthy state");
    wait_for_health("http://127.0.0.1:8082/health", timeout)
        .await
        .expect("Instance 2 failed to reach healthy state");

    let client = reqwest::Client::new();

    // Verify health payload and W3C trace context headers
    let h1_resp = client
        .get("http://127.0.0.1:8081/health")
        .send()
        .await
        .expect("health 1 failed");
    assert_eq!(h1_resp.status(), 200);
    assert!(h1_resp.headers().contains_key("x-request-id"));
    assert!(h1_resp.headers().contains_key("traceparent"));

    let h2_resp = client
        .get("http://127.0.0.1:8082/health")
        .send()
        .await
        .expect("health 2 failed");
    assert_eq!(h2_resp.status(), 200);
    assert!(h2_resp.headers().contains_key("x-request-id"));
    assert!(h2_resp.headers().contains_key("traceparent"));

    // Connect to PostgreSQL to verify migrations
    let pg_url = "postgres://postgres:postgres@127.0.0.1:5433/workbench";
    let pool = sqlx::PgPool::connect(pg_url)
        .await
        .expect("failed to connect to postgres container");

    let migration_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM rume_migrations")
        .fetch_one(&pool)
        .await
        .expect("failed to count applied migrations");
    assert!(
        migration_count.0 >= 3,
        "expected at least 3 applied migrations, found {}",
        migration_count.0
    );

    // =========================================================================
    // Scenario 2: Two-Instance Operation
    // Both processes serve requests and see consistent persisted state.
    // =========================================================================
    println!("[compose_system_test] Scenario 2: Two-Instance Operation...");
    let org_payload = json!({
        "id": "org-stark",
        "name": "Stark Industries"
    });

    let create_org_resp = client
        .post("http://127.0.0.1:8081/api/v1/organizations")
        .json(&org_payload)
        .send()
        .await
        .expect("failed to create organization on instance 1");
    assert_eq!(create_org_resp.status(), 201);

    // Read organization on Instance 2
    let get_org_resp = client
        .get("http://127.0.0.1:8082/api/v1/organizations/org-stark")
        .send()
        .await
        .expect("failed to get organization on instance 2");
    assert_eq!(get_org_resp.status(), 200);
    let org_data: serde_json::Value = get_org_resp.json().await.expect("invalid json");
    assert_eq!(org_data["name"], "Stark Industries");

    // Create user on Instance 1 (using pre-seeded admin-token for acme-corp)
    let user_payload = json!({
        "id": "user-stark-tony",
        "email": "tony@stark.com",
        "name": "Tony Stark",
        "role": "member",
        "api_token": "token-tony-ironman"
    });
    let create_user_resp = client
        .post("http://127.0.0.1:8081/api/v1/users")
        .header("X-Tenant-Id", "acme-corp")
        .header("Authorization", "Bearer admin-token")
        .json(&user_payload)
        .send()
        .await
        .expect("failed to create user on instance 1");
    assert_eq!(create_user_resp.status(), 201);

    // Read user on Instance 2 with tenant authentication
    let get_user_resp = client
        .get("http://127.0.0.1:8082/api/v1/users/user-stark-tony")
        .header("X-Tenant-Id", "acme-corp")
        .header("Authorization", "Bearer token-tony-ironman")
        .send()
        .await
        .expect("failed to get user on instance 2");
    assert_eq!(get_user_resp.status(), 200);
    let user_data: serde_json::Value = get_user_resp.json().await.expect("invalid json");
    assert_eq!(user_data["name"], "Tony Stark");

    // =========================================================================
    // Scenario 3: Cross-Instance Jobs & Transactional Outbox Processing
    // Event written by instance 1 is processed by background worker across instances.
    // =========================================================================
    println!("[compose_system_test] Scenario 3: Cross-Instance Jobs & Outbox Processing...");
    // Create project on Instance 1
    let project_payload = json!({
        "name": "Project Mark 42",
        "description": "Autonomous armor propulsion"
    });
    let create_proj_resp = client
        .post("http://127.0.0.1:8081/api/v1/projects")
        .header("X-Tenant-Id", "acme-corp")
        .header("Authorization", "Bearer admin-token")
        .json(&project_payload)
        .send()
        .await
        .expect("failed to create project on instance 1");
    assert_eq!(create_proj_resp.status(), 201);
    let proj_data: serde_json::Value = create_proj_resp.json().await.expect("invalid project json");
    let proj_id = proj_data["id"].as_str().expect("project id missing");

    // Create issue on Instance 1 (atomically inserts issue and enqueues outbox message)
    let issue_payload = json!({
        "title": "Avionics calibration",
        "description": "Flight stabilization telemetry",
        "priority": "urgent",
        "assignee_id": "user-stark-tony"
    });
    let create_issue_resp = client
        .post(format!("http://127.0.0.1:8081/api/v1/projects/{proj_id}/issues"))
        .header("X-Tenant-Id", "acme-corp")
        .header("Authorization", "Bearer admin-token")
        .json(&issue_payload)
        .send()
        .await
        .expect("failed to create issue on instance 1");
    assert_eq!(create_issue_resp.status(), 201);
    let issue_data: serde_json::Value = create_issue_resp.json().await.expect("invalid issue json");
    let issue_id = issue_data["id"].as_str().expect("issue id missing");

    // Wait for outbox relay and background job worker to process the event
    let start_wait = std::time::Instant::now();
    let mut outbox_published = false;
    let mut job_completed = false;
    let job_idempotency_key = format!("notify-issue-{}", issue_id);

    while start_wait.elapsed() < Duration::from_secs(15) {
        let outbox_rows: Vec<(String, Vec<u8>)> = sqlx::query_as(
            "SELECT status, payload FROM rume_outbox WHERE event_type = 'issue.created'"
        )
        .fetch_all(&pool)
        .await
        .expect("query outbox failed");

        for (status, payload) in outbox_rows {
            if payload.windows(issue_id.len()).any(|w| w == issue_id.as_bytes()) && status == "published" {
                outbox_published = true;
            }
        }

        let job_row: Option<(String,)> = sqlx::query_as(
            "SELECT status FROM rume_jobs WHERE name = 'send_notification' AND idempotency_key = $1"
        )
        .bind(&job_idempotency_key)
        .fetch_optional(&pool)
        .await
        .expect("query jobs failed");

        if let Some((status,)) = job_row
            && status == "completed"
        {
            job_completed = true;
        }

        if outbox_published && job_completed {
            break;
        }

        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    assert!(outbox_published, "Outbox event for {issue_id} was not published by relay");
    assert!(job_completed, "Notification job for {issue_id} was not completed by worker");

    // =========================================================================
    // Scenario 4: Instance Failure
    // Killing one process does not lose committed data or durable work;
    // surviving instance continues serving and processing jobs.
    // =========================================================================
    println!("[compose_system_test] Scenario 4: Instance Failure...");
    let kill_out = guard.run_cmd(&["kill", "workbench-1"]);
    assert!(kill_out.status.success(), "failed to kill workbench-1");

    // Verify Instance 1 is down
    let inst1_check = client.get("http://127.0.0.1:8081/health").send().await;
    assert!(
        inst1_check.is_err(),
        "Instance 1 should not respond after SIGKILL"
    );

    // Create a new issue on surviving Instance 2
    let issue2_payload = json!({
        "title": "Thruster assembly check",
        "description": "Checked while instance 1 is dead",
        "priority": "high",
        "assignee_id": "user-stark-tony"
    });
    let create_issue2_resp = client
        .post(format!("http://127.0.0.1:8082/api/v1/projects/{proj_id}/issues"))
        .header("X-Tenant-Id", "acme-corp")
        .header("Authorization", "Bearer admin-token")
        .json(&issue2_payload)
        .send()
        .await
        .expect("failed to create issue on instance 2");
    assert_eq!(create_issue2_resp.status(), 201);
    let issue2_data: serde_json::Value = create_issue2_resp.json().await.expect("invalid issue 2 json");
    let issue2_id = issue2_data["id"].as_str().expect("issue 2 id missing");

    // Verify Instance 2 processes the outbox event and job alone
    let start_wait2 = std::time::Instant::now();
    let mut outbox2_published = false;
    let mut job2_completed = false;
    let job2_idempotency_key = format!("notify-issue-{}", issue2_id);

    while start_wait2.elapsed() < Duration::from_secs(15) {
        let outbox_rows: Vec<(String, Vec<u8>)> = sqlx::query_as(
            "SELECT status, payload FROM rume_outbox WHERE event_type = 'issue.created'"
        )
        .fetch_all(&pool)
        .await
        .expect("query outbox 2 failed");

        for (status, payload) in outbox_rows {
            if payload.windows(issue2_id.len()).any(|w| w == issue2_id.as_bytes()) && status == "published" {
                outbox2_published = true;
            }
        }

        let job_row: Option<(String,)> = sqlx::query_as(
            "SELECT status FROM rume_jobs WHERE name = 'send_notification' AND idempotency_key = $1"
        )
        .bind(&job2_idempotency_key)
        .fetch_optional(&pool)
        .await
        .expect("query jobs 2 failed");

        if let Some((status,)) = job_row
            && status == "completed"
        {
            job2_completed = true;
        }

        if outbox2_published && job2_completed {
            break;
        }

        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    assert!(outbox2_published, "Instance 2 failed to publish outbox event while Instance 1 was down");
    assert!(job2_completed, "Instance 2 failed to complete notification job while Instance 1 was down");

    // =========================================================================
    // Scenario 5: Restart Recovery
    // Pending work and state remain intact and recovered after process restart.
    // =========================================================================
    println!("[compose_system_test] Scenario 5: Restart Recovery...");
    let restart_out = guard.run_cmd(&["up", "-d", "workbench-1"]);
    assert!(restart_out.status.success(), "failed to restart workbench-1");

    wait_for_health("http://127.0.0.1:8081/health", timeout)
        .await
        .expect("Instance 1 failed to recover after restart");

    // Verify recovered Instance 1 can read the issue created while it was dead
    let get_issue2_resp = client
        .get(format!("http://127.0.0.1:8081/api/v1/issues/{issue2_id}"))
        .header("X-Tenant-Id", "acme-corp")
        .header("Authorization", "Bearer admin-token")
        .send()
        .await
        .expect("failed to read issue from recovered instance 1");
    assert_eq!(get_issue2_resp.status(), 200);
    let issue2_retrieved: serde_json::Value = get_issue2_resp.json().await.expect("invalid json");
    assert_eq!(issue2_retrieved["title"], "Thruster assembly check");

    // =========================================================================
    // Scenario 6: Dependency Restart
    // PostgreSQL or Redis restarts and the application recovers according to its contract.
    // =========================================================================
    println!("[compose_system_test] Scenario 6: Dependency Restart...");
    // Restart Redis
    let restart_redis = guard.run_cmd(&["restart", "redis"]);
    assert!(restart_redis.status.success(), "failed to restart redis");
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Both instances still respond
    let r1 = client.get("http://127.0.0.1:8081/health").send().await.expect("inst 1 after redis restart");
    let r2 = client.get("http://127.0.0.1:8082/health").send().await.expect("inst 2 after redis restart");
    assert_eq!(r1.status(), 200);
    assert_eq!(r2.status(), 200);

    // =========================================================================
    // Scenario 7: Graceful Shutdown
    // SIGTERM drains active requests, workers and relays correctly.
    // =========================================================================
    println!("[compose_system_test] Scenario 7: Graceful Shutdown...");
    let stop_out = guard.run_cmd(&["stop", "-t", "10", "workbench-2"]);
    assert!(stop_out.status.success(), "failed to gracefully stop workbench-2");

    let logs_out = guard.run_cmd(&["logs", "workbench-2"]);
    let log_str = String::from_utf8_lossy(&logs_out.stdout);

    assert!(
        log_str.contains("Received SIGTERM, initiating graceful shutdown"),
        "Missing SIGTERM receipt in logs: {}",
        log_str
    );
    assert!(
        log_str.contains("Shutting down Workbench background workers, relay, and connection pool..."),
        "Missing shutdown drain initiation in logs: {}",
        log_str
    );
    assert!(
        log_str.contains("Workbench stopped cleanly."),
        "Missing clean shutdown completion in logs: {}",
        log_str
    );

    println!("[compose_system_test] All 8 Docker Compose qualification scenarios PASSED successfully!");
}
