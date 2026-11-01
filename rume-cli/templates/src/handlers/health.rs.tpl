use crate::config::Config;
use crate::env::Env;
use rume::app::AppContext;
use rume::prelude::*;

#[derive(Serialize)]
pub struct Health {
    pub status: &'static str,
    pub app_name: String,
}

/// Custom health handler example.
///
/// Note: `/health/live` and `/health/ready` are already handled by
/// `rume::routing::health_routes()` in `src/app.rs`. This handler is an
/// example of a custom health endpoint you can add.
pub async fn health(State(ctx): State<AppContext<Env, Config>>) -> Json<Health> {
    Json(Health {
        status: "ok",
        app_name: ctx.config.app.name.clone(),
    })
}
