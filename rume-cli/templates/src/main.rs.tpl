use rume::app::Rume;
use rume::axum::{Json, Router, routing::get};
use rume::serde::Serialize;

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let router = Router::new().route("/health", get(|| async { Json(Health { status: "ok" }) }));

    Rume::builder()
        .router(router)
        .bind(([127, 0, 0, 1], 3333))
        .run()
        .await
}
