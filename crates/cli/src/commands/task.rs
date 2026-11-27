//! Task inspection and execution commands for `rume task`.
//!
//! Boots the application in `ApplicationMode::Task` via `cargo run --quiet`
//! to list registered tasks or execute a named task with full `AppContext`.

use std::process::Command;

use rume::app::TaskInfo;

use crate::cli::TaskCommands;
use crate::ui::table::TableBuilder;

const TASKS_START_TAG: &str = "__RUME_TASKS_JSON_START__";
const TASKS_END_TAG: &str = "__RUME_TASKS_JSON_END__";

pub fn parse_tasks_json(stdout: &str) -> Result<Vec<TaskInfo>, String> {
    let json_str = stdout
        .find(TASKS_START_TAG)
        .and_then(|start| {
            let after_start = &stdout[start + TASKS_START_TAG.len()..];
            after_start
                .find(TASKS_END_TAG)
                .map(|end| after_start[..end].trim())
        })
        .ok_or_else(|| {
            format!("application did not produce task inspection metadata.\n{stdout}")
        })?;

    serde_json::from_str(json_str)
        .map_err(|err| format!("failed to parse task inspection JSON: {err}\nPayload: {json_str}"))
}

pub fn run(command: TaskCommands) {
    match command {
        TaskCommands::List { bin, format, json } => {
            let is_json = json || format == "json";
            run_list(bin.as_deref(), is_json);
        }
        TaskCommands::Run { name, bin } => {
            run_task(&name, bin.as_deref());
        }
    }
}

fn run_list(bin: Option<&str>, json_output: bool) {
    let mut args = vec!["run", "--quiet"];
    if let Some(b) = bin {
        args.push("--bin");
        args.push(b);
    }
    args.extend(["--", "--rume-list-tasks"]);

    let output = match Command::new("cargo")
        .args(&args)
        .env("RUME_TASK_LIST", "1")
        .output()
    {
        Ok(o) => o,
        Err(err) => {
            eprintln!("error: failed to execute cargo to list tasks: {err}");
            std::process::exit(1);
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        eprintln!("error: failed to list application tasks.\n{stderr}\n{stdout}");
        std::process::exit(output.status.code().unwrap_or(1));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let tasks = match parse_tasks_json(&stdout) {
        Ok(t) => t,
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    };

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&tasks).unwrap_or_else(|_| "[]".to_string())
        );
        return;
    }

    if tasks.is_empty() {
        println!("No tasks registered in application.");
        return;
    }

    let mut table = TableBuilder::new();
    table.head(&["NAME", "DESCRIPTION"]);
    for task in &tasks {
        table.row(&[&task.name, &task.description]);
    }
    table.render();
}

fn run_task(name: &str, bin: Option<&str>) {
    let mut args = vec!["run", "--quiet"];
    if let Some(b) = bin {
        args.push("--bin");
        args.push(b);
    }
    args.extend(["--", "--rume-task", name]);

    let status = match Command::new("cargo")
        .args(&args)
        .env("RUME_TASK", name)
        .status()
    {
        Ok(s) => s,
        Err(err) => {
            eprintln!("error: failed to execute cargo to run task '{name}': {err}");
            std::process::exit(1);
        }
    };

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tasks_json_extracts_tagged_payload() {
        let stdout = "\
booting app...\n\
__RUME_TASKS_JSON_START__\n\
[{\"name\":\"cleanup\",\"description\":\"Remove expired sessions\"}]\n\
__RUME_TASKS_JSON_END__\n\
done\n";

        let tasks = parse_tasks_json(stdout).expect("tagged JSON should parse");
        assert_eq!(
            tasks,
            vec![TaskInfo {
                name: "cleanup".to_string(),
                description: "Remove expired sessions".to_string(),
            }]
        );

        let missing = parse_tasks_json("no tags here");
        assert!(missing.is_err());
    }
}
