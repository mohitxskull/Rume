//! List registered routes for `rume list:routes`.
//!
//! Reads `src/routes.rs` and extracts route path strings from `.route()`
//! calls. This avoids booting the full application just to inspect routes.

use std::fs;

pub fn run() {
    let routes_path = "src/routes.rs";
    let content = match fs::read_to_string(routes_path) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("error: could not read {routes_path}: {err}");
            std::process::exit(1);
        }
    };

    let mut routes: Vec<(String, String)> = Vec::new();

    // Simple extraction: find .route("/path", ...) patterns.
    for line in content.lines() {
        if let Some((method, path)) = extract_route(line) {
            routes.push((method.to_string(), path.to_string()));
        }
    }

    if routes.is_empty() {
        println!("No routes found in {routes_path}.");
        return;
    }

    println!("Registered routes:");
    println!("{:<10} PATH", "METHOD");
    println!("{:-<10} {:-<40}", "", "");
    for (method, path) in &routes {
        println!("{:<10} {}", method, path);
    }
}

/// Extract an HTTP method and path from a `.route("/path", method_router)` call.
///
/// Returns `(method, path)` where method is derived from the router variant
/// (e.g. `get` → `GET`, `post` → `POST`) or `"ANY"` if unrecognized.
fn extract_route(line: &str) -> Option<(&str, &str)> {
    // Match patterns like: router.route("/users", axum::routing::get(handler))
    // or: router = router.route("/posts", get(handler))
    let route_idx = line.find(".route(")?;
    let after_route = &line[route_idx + 6..]; // skip ".route("

    // Find the first string literal (the path)
    let path_start = after_route.find('"')?;
    let path_end = after_route[path_start + 1..].find('"')?;
    let path = &after_route[path_start + 1..path_start + 1 + path_end];

    // After the path, find the router call to determine the method
    let after_path = &after_route[path_start + 1 + path_end + 1..]; // skip closing quote and comma/space
    let method = extract_method(after_path);

    Some((method, path))
}

/// Determine the HTTP method from the router expression after the path.
fn extract_method(after_path: &str) -> &str {
    let trimmed = after_path.trim();
    if trimmed.starts_with("axum::routing::get") || trimmed.starts_with("get(") {
        "GET"
    } else if trimmed.starts_with("axum::routing::post") || trimmed.starts_with("post(") {
        "POST"
    } else if trimmed.starts_with("axum::routing::put") || trimmed.starts_with("put(") {
        "PUT"
    } else if trimmed.starts_with("axum::routing::patch") || trimmed.starts_with("patch(") {
        "PATCH"
    } else if trimmed.starts_with("axum::routing::delete") || trimmed.starts_with("delete(") {
        "DELETE"
    } else {
        "ANY"
    }
}
