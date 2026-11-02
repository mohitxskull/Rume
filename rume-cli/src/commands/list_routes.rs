//! List registered routes for `rume list:routes`.
//!
//! Boots the application in `Inspect` mode with `RUME_INSPECT=routes`
//! and extracts the canonical route inventory from the application's
//! [`rume::routing::RouteRegistry`].
//!
//! This completely eliminates source-code heuristic parsing and ensures
//! nested, grouped, resource, and merged routes are represented with 100%
//! runtime fidelity.

use crate::ui::table::TableBuilder;
use comfy_table::Color;
use rume::routing::RouteInfo;
use std::process::Command;

pub fn run(json_output: bool) {
    let output = match Command::new("cargo")
        .args(["run", "--quiet", "--", "--rume-inspect-routes"])
        .env("RUME_INSPECT", "routes")
        .output()
    {
        Ok(o) => o,
        Err(err) => {
            eprintln!("error: failed to execute cargo to inspect routes: {err}");
            std::process::exit(1);
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        eprintln!("error: failed to inspect application routes.\n{stderr}\n{stdout}");
        std::process::exit(output.status.code().unwrap_or(1));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let start_tag = "__RUME_ROUTES_JSON_START__";
    let end_tag = "__RUME_ROUTES_JSON_END__";

    let json_str = match stdout.find(start_tag).and_then(|start| {
        let after_start = &stdout[start + start_tag.len()..];
        after_start
            .find(end_tag)
            .map(|end| after_start[..end].trim())
    }) {
        Some(s) => s,
        None => {
            eprintln!("error: application did not produce route inspection metadata.\n{stdout}");
            std::process::exit(1);
        }
    };

    let routes: Vec<RouteInfo> = match serde_json::from_str(json_str) {
        Ok(r) => r,
        Err(err) => {
            eprintln!("error: failed to parse route inspection JSON: {err}\nPayload: {json_str}");
            std::process::exit(1);
        }
    };

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&routes).unwrap_or_else(|_| "[]".to_string())
        );
        return;
    }

    if routes.is_empty() {
        println!("No routes registered in application.");
        return;
    }

    let mut table = TableBuilder::new();
    table.head(&["METHOD", "NAME", "PARAMS", "PATH"]);

    for route in &routes {
        let method_str = route.method.as_str();
        let color = match method_str {
            "GET" => Some(Color::Green),
            "POST" => Some(Color::Blue),
            "PUT" => Some(Color::Yellow),
            "PATCH" => Some(Color::Cyan),
            "DELETE" => Some(Color::Red),
            _ => None,
        };

        let name = route.name.as_deref().unwrap_or("-");
        let params = if route.params.is_empty() {
            "[]".to_string()
        } else {
            format!("[{}]", route.params.join(", "))
        };

        table.row_colored(&[
            (method_str, color),
            (name, None),
            (&params, None),
            (&route.path, None),
        ]);
    }

    table.render();
}
