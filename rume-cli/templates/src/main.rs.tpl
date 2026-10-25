use rume::axum::{Json, Router, routing::get};
use rume::serde::Serialize;

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

#[tokio::main]
async fn main() {
    let app = Router::new().route("/health", get(|| async { Json(Health { status: "ok" }) }));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3333")
        .await
        .expect("failed to bind address");

    println!("listening on http://127.0.0.1:3333");
    rume::axum::serve(listener, app).await.expect("server error");
}
