//! Live-reload development server for `rume dev`.
//!
//! Uses a pure-Rust debounced polling watcher (`std::fs::metadata`) instead of
//! OS-specific notification crates for deterministic behavior across Linux,
//! WSL, containers, and network filesystems with zero extra dependencies.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime};

use console::style;

const DEFAULT_WATCH_PATHS: &[&str] = &["src", "config", "migrations", "sql", "Cargo.toml"];
const IGNORED_DIRS: &[&str] = &["target", ".git", ".hg", ".svn"];

pub async fn run(bin: Option<String>, watch: Vec<PathBuf>, no_migrate: bool, debounce_ms: u64) {
    if !no_migrate && Path::new("migrations").is_dir() {
        println!("{}", style("Running pending database migrations...").cyan());
        crate::commands::db::migrate().await;
    }

    let explicit_watch = !watch.is_empty();
    let mut watch_paths = resolve_watch_paths(watch);
    if watch_paths.is_empty() {
        if explicit_watch {
            eprintln!(
                "{}",
                style("error: none of the specified --watch paths exist").red()
            );
            std::process::exit(1);
        }
        watch_paths.push(PathBuf::from("."));
    }

    let build_args = build_cargo_build_args(bin.as_deref());
    println!(
        "{}",
        style(format!(
            "Starting dev server (`cargo {}`), watching {} path(s)...",
            build_args.join(" "),
            watch_paths.len()
        ))
        .green()
        .bold()
    );

    let mut child = match build_and_spawn_server(bin.as_deref()).await {
        Ok(c) => Some(c),
        Err(err) => {
            eprintln!("{}", style(format!("error: {err}")).red());
            std::process::exit(1);
        }
    };

    let mut prev_snapshot = snapshot_tree(&watch_paths);
    let mut debouncer = Debouncer::new(Duration::from_millis(debounce_ms));
    let poll_ms = (debounce_ms / 3).clamp(50, 200);
    let mut tick = tokio::time::interval(Duration::from_millis(poll_ms));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\n{}", style("Shutting down dev server...").yellow());
                if let Some(ref mut c) = child {
                    terminate_child(c).await;
                }
                break;
            }
            res = wait_for_child(&mut child) => {
                child = None;
                match res {
                    Ok(status) => {
                        println!(
                            "{}",
                            style(format!("Server exited ({status}); waiting for file changes..."))
                                .yellow()
                        );
                    }
                    Err(err) => {
                        eprintln!(
                            "{}",
                            style(format!("error: failed waiting on server process: {err}")).red()
                        );
                    }
                }
            }
            _ = tick.tick() => {
                let now = Instant::now();
                let next_snapshot = snapshot_tree(&watch_paths);
                let changed = diff_snapshots(&prev_snapshot, &next_snapshot);
                if !changed.is_empty() {
                    prev_snapshot = next_snapshot;
                    debouncer.record_event(now);
                }
                if debouncer.should_fire(now) {
                    println!(
                        "{}",
                        style("Change detected, restarting server...").cyan().bold()
                    );
                    if let Some(ref mut c) = child {
                        terminate_child(c).await;
                    }
                    match build_and_spawn_server(bin.as_deref()).await {
                        Ok(c) => child = Some(c),
                        Err(err) => {
                            eprintln!("{}", style(format!("error: {err}")).red());
                            child = None;
                        }
                    }
                }
            }
        }
    }
}

fn resolve_watch_paths(watch: Vec<PathBuf>) -> Vec<PathBuf> {
    if watch.is_empty() {
        DEFAULT_WATCH_PATHS
            .iter()
            .map(PathBuf::from)
            .filter(|p| p.exists())
            .collect()
    } else {
        watch.into_iter().filter(|p| p.exists()).collect()
    }
}

/// Builds the argument list for `cargo build --message-format=json-render-diagnostics`.
pub fn build_cargo_build_args(bin: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "build".to_string(),
        "--message-format=json-render-diagnostics".to_string(),
    ];
    if let Some(name) = bin {
        args.push("--bin".to_string());
        args.push(name.to_string());
    }
    args
}

/// Parses `cargo build --message-format=json-render-diagnostics` stdout to locate the compiled
/// binary executable.
fn parse_cargo_executable_from_json(stdout: &str, bin: Option<&str>) -> Option<PathBuf> {
    let mut fallback: Option<PathBuf> = None;
    for line in stdout.lines() {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if msg.get("reason").and_then(serde_json::Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let is_bin = msg
            .get("target")
            .and_then(|t| t.get("kind"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|kinds| kinds.iter().any(|k| k.as_str() == Some("bin")));
        if !is_bin {
            continue;
        }
        let Some(executable) = msg.get("executable").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let target_name = msg
            .get("target")
            .and_then(|t| t.get("name"))
            .and_then(serde_json::Value::as_str);
        if let Some(wanted) = bin {
            if target_name == Some(wanted) {
                return Some(PathBuf::from(executable));
            }
        } else {
            fallback = Some(PathBuf::from(executable));
        }
    }
    fallback
}

/// Builds the server with `cargo build --message-format=json` and spawns the compiled
/// executable directly so `child.kill()` terminates the actual server process instead of
/// only killing a `cargo run` wrapper and orphaning the grandchild server under
/// `#![forbid(unsafe_code)]`.
async fn build_and_spawn_server(bin: Option<&str>) -> Result<tokio::process::Child, String> {
    let build_args = build_cargo_build_args(bin);
    let output = tokio::process::Command::new("cargo")
        .args(&build_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .await
        .map_err(|err| format!("failed to run cargo build: {err}"))?;

    if !output.status.success() {
        return Err(format!("cargo build failed ({})", output.status));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Some(executable) = parse_cargo_executable_from_json(&stdout, bin) {
        return tokio::process::Command::new(&executable)
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| {
                format!(
                    "failed to spawn compiled server binary '{}': {err}",
                    executable.display()
                )
            });
    }

    // Why: fall back to `cargo run` only if `cargo build --message-format=json` emitted no
    // executable artifact (e.g. custom cargo wrapper or workspace without a default bin).
    let run_args = build_cargo_run_args(bin);
    spawn_server(&run_args).map_err(|err| format!("failed to spawn cargo run: {err}"))
}

fn spawn_server(args: &[String]) -> std::io::Result<tokio::process::Child> {
    tokio::process::Command::new("cargo")
        .args(args)
        .kill_on_drop(true)
        .spawn()
}

async fn wait_for_child(
    child: &mut Option<tokio::process::Child>,
) -> std::io::Result<std::process::ExitStatus> {
    match child.as_mut() {
        Some(c) => c.wait().await,
        None => std::future::pending().await,
    }
}

async fn terminate_child(child: &mut tokio::process::Child) {
    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// Returns `true` for VCS/build directories and editor swap/backup files.
pub fn should_ignore_path(path: &Path) -> bool {
    for comp in path.components() {
        if let Component::Normal(part) = comp
            && let Some(s) = part.to_str()
            && IGNORED_DIRS.contains(&s)
        {
            return true;
        }
    }

    if let Some(name) = path.file_name().and_then(|n| n.to_str())
        && (name.ends_with(".swp")
            || name.ends_with(".swo")
            || name.ends_with('~')
            || name.starts_with(".#"))
    {
        return true;
    }

    false
}

/// Walks `roots` recursively and records `(len, modified)` for every non-ignored file.
pub fn snapshot_tree(roots: &[PathBuf]) -> BTreeMap<PathBuf, (u64, SystemTime)> {
    let mut snapshot = BTreeMap::new();
    for root in roots {
        collect_snapshot(root, &mut snapshot);
    }
    snapshot
}

fn collect_snapshot(path: &Path, out: &mut BTreeMap<PathBuf, (u64, SystemTime)>) {
    // Why: `walkdir::WalkDir` with `follow_links(false)` prevents directory symlink
    // cycles from causing unbounded recursion while `filter_entry` prunes ignored
    // directories (e.g. `target/`, `.git/`) before descending into them.
    for entry in walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !should_ignore_path(e.path()))
        .flatten()
    {
        if entry.file_type().is_file()
            && let Ok(meta) = entry.metadata()
        {
            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            out.insert(entry.into_path(), (meta.len(), modified));
        }
    }
}

/// Returns a deterministic list of created, modified, or deleted paths between two snapshots.
pub fn diff_snapshots(
    prev: &BTreeMap<PathBuf, (u64, SystemTime)>,
    next: &BTreeMap<PathBuf, (u64, SystemTime)>,
) -> Vec<PathBuf> {
    let mut changed = BTreeSet::new();

    for (path, meta) in next {
        if prev.get(path) != Some(meta) {
            changed.insert(path.clone());
        }
    }

    for path in prev.keys() {
        if !next.contains_key(path) {
            changed.insert(path.clone());
        }
    }

    changed.into_iter().collect()
}

/// Coalesces rapid change events into a single trigger after `window` of quiet time.
pub struct Debouncer {
    window: Duration,
    last_event: Option<Instant>,
}

impl Debouncer {
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            last_event: None,
        }
    }

    pub fn record_event(&mut self, at: Instant) {
        self.last_event = Some(at);
    }

    pub fn should_fire(&mut self, now: Instant) -> bool {
        match self.last_event {
            Some(last) if now.saturating_duration_since(last) >= self.window => {
                self.last_event = None;
                true
            }
            _ => false,
        }
    }
}

/// Builds the argument list for `cargo` (`["run"]` or `["run", "--bin", <name>]`).
pub fn build_cargo_run_args(bin: Option<&str>) -> Vec<String> {
    let mut args = vec!["run".to_string()];
    if let Some(name) = bin {
        args.push("--bin".to_string());
        args.push(name.to_string());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn ignore_filter_skips_target_git_and_swap_files() {
        assert!(should_ignore_path(Path::new("target/debug/app")));
        assert!(should_ignore_path(Path::new(".git/HEAD")));
        assert!(should_ignore_path(Path::new("src/.main.rs.swp")));
        assert!(should_ignore_path(Path::new("src/main.rs~")));
        assert!(should_ignore_path(Path::new(".hg/store")));
        assert!(should_ignore_path(Path::new(".svn/entries")));

        assert!(!should_ignore_path(Path::new("src/main.rs")));
        assert!(!should_ignore_path(Path::new("config/app.toml")));
        assert!(!should_ignore_path(Path::new("migrations/001.up.sql")));
    }

    #[test]
    fn snapshot_diff_detects_create_modify_and_delete() {
        let dir = TempDir::new().expect("create temp dir");
        let root = dir.path().to_path_buf();

        let src_dir = root.join("src");
        let target_dir = root.join("target");
        fs::create_dir_all(&src_dir).expect("create src dir");
        fs::create_dir_all(&target_dir).expect("create target dir");

        let main_rs = src_dir.join("main.rs");
        let lib_rs = src_dir.join("lib.rs");
        let ignored_bin = target_dir.join("app");

        fs::write(&main_rs, "fn main() {}").expect("write main.rs");
        fs::write(&lib_rs, "pub fn a() {}").expect("write lib.rs");
        fs::write(&ignored_bin, "binary").expect("write target/app");

        let snap0 = snapshot_tree(std::slice::from_ref(&root));
        assert!(snap0.contains_key(&main_rs));
        assert!(snap0.contains_key(&lib_rs));
        assert!(!snap0.contains_key(&ignored_bin));

        // Create a new file, modify an existing file (different length), delete another.
        let extra_rs = src_dir.join("extra.rs");
        fs::write(&extra_rs, "pub fn extra() {}").expect("create extra.rs");
        fs::write(&main_rs, "fn main() { println!(\"hi\"); }").expect("modify main.rs");
        fs::remove_file(&lib_rs).expect("delete lib.rs");
        fs::write(&ignored_bin, "updated binary").expect("modify ignored target/app");

        let snap1 = snapshot_tree(std::slice::from_ref(&root));
        let diff = diff_snapshots(&snap0, &snap1);

        assert!(diff.contains(&extra_rs), "expected created file in diff");
        assert!(diff.contains(&main_rs), "expected modified file in diff");
        assert!(diff.contains(&lib_rs), "expected deleted file in diff");
        assert!(
            !diff.contains(&ignored_bin),
            "target/ file must remain ignored"
        );
    }

    #[test]
    fn debouncer_coalesces_events_within_window() {
        let mut debouncer = Debouncer::new(Duration::from_millis(300));
        let t0 = Instant::now();

        debouncer.record_event(t0);
        assert!(!debouncer.should_fire(t0 + Duration::from_millis(100)));

        debouncer.record_event(t0 + Duration::from_millis(150));
        assert!(!debouncer.should_fire(t0 + Duration::from_millis(350)));

        assert!(debouncer.should_fire(t0 + Duration::from_millis(450)));
        assert!(!debouncer.should_fire(t0 + Duration::from_millis(600)));
    }

    #[test]
    fn build_cargo_run_args_handles_default_and_named_bin() {
        assert_eq!(build_cargo_run_args(None), vec!["run".to_string()]);
        assert_eq!(
            build_cargo_run_args(Some("api")),
            vec!["run".to_string(), "--bin".to_string(), "api".to_string()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_tree_handles_directory_symlink_cycle_without_overflow() {
        let dir = TempDir::new().expect("create temp dir");
        let root = dir.path().to_path_buf();
        let src_dir = root.join("src");
        fs::create_dir_all(&src_dir).expect("create src dir");
        let main_rs = src_dir.join("main.rs");
        fs::write(&main_rs, "fn main() {}").expect("write main.rs");

        // Create a directory symlink loop: src/loop -> root
        std::os::unix::fs::symlink(&root, src_dir.join("loop")).expect("create symlink loop");

        let snap = snapshot_tree(std::slice::from_ref(&root));
        assert!(snap.contains_key(&main_rs));
    }

    #[test]
    fn build_cargo_build_args_uses_json_render_diagnostics() {
        assert_eq!(
            build_cargo_build_args(None),
            vec![
                "build".to_string(),
                "--message-format=json-render-diagnostics".to_string()
            ]
        );
        assert_eq!(
            build_cargo_build_args(Some("api")),
            vec![
                "build".to_string(),
                "--message-format=json-render-diagnostics".to_string(),
                "--bin".to_string(),
                "api".to_string()
            ]
        );
    }

    #[test]
    fn parse_cargo_executable_from_json_selects_matching_bin() {
        let stdout = concat!(
            r#"{"reason":"compiler-artifact","target":{"name":"build-script-build","kind":["custom-build"]},"executable":"/tmp/target/debug/build/foo/build-script-build"}"#,
            "\n",
            r#"{"reason":"compiler-artifact","target":{"name":"helper","kind":["bin"]},"executable":"/tmp/target/debug/helper"}"#,
            "\n",
            r#"{"reason":"compiler-artifact","target":{"name":"api","kind":["bin"]},"executable":"/tmp/target/debug/api"}"#,
            "\n",
            r#"{"reason":"compiler-artifact","target":{"name":"post-build","kind":["custom-build"]},"executable":"/tmp/target/debug/build/bar/build-script-build"}"#,
            "\n"
        );
        assert_eq!(
            parse_cargo_executable_from_json(stdout, Some("helper")),
            Some(PathBuf::from("/tmp/target/debug/helper"))
        );
        assert_eq!(
            parse_cargo_executable_from_json(stdout, None),
            Some(PathBuf::from("/tmp/target/debug/api"))
        );
        assert_eq!(
            parse_cargo_executable_from_json(stdout, Some("missing")),
            None
        );
        assert_eq!(
            parse_cargo_executable_from_json(stdout, Some("build-script-build")),
            None
        );
    }
}
