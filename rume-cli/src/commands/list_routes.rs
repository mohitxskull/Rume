//! List registered routes for `rume list:routes`.
//!
//! Reads `src/routes.rs` and extracts route path strings from `.route()`
//! calls. This avoids booting the full application just to inspect routes.
//! Multiline `.route()` calls are joined before parsing.

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

    // Join multiline .route() calls: accumulate lines until a line ends
    // with ')' (the closing of .route(...)). This handles routes spread
    // across multiple lines, e.g. composed routers.
    let mut accumulator = String::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        if !accumulator.is_empty() {
            accumulator.push(' ');
        }
        accumulator.push_str(trimmed);

        // When we see a complete .route(...) call, extract it.
        if accumulator.contains(".route(") && accumulator.ends_with(')') {
            if let Some((method, path)) = extract_route(&accumulator) {
                routes.push((method.to_string(), path.to_string()));
            }
            accumulator.clear();
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
fn extract_route(call: &str) -> Option<(&str, &str)> {
    // Match patterns like: router.route("/users", axum::routing::get(handler))
    // or: router = router.route("/posts", get(handler))
    let route_idx = call.find(".route(")?;
    let after_route = &call[route_idx + 7..]; // skip ".route("

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
