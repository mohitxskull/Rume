use axum::{extract::Request, middleware::Next, response::Response};

pub async fn handle(request: Request, next: Next) -> Response {
    // Add middleware logic here.
    next.run(request).await
}
