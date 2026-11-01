use crate::config::Config;
use crate::env::Env;
use rume::app::AppContext;
use rume::prelude::*;

#[derive(Serialize)]
pub struct Health {
    pub status: &'static str,
    pub app_name: String,
}

pub async fn health(State(ctx): State<AppContext<Env, Config>>) -> Json<Health> {
    Json(Health {
        status: "ok",
        app_name: ctx.config.app.name.clone(),
    })
}
