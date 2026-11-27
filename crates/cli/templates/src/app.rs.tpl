use crate::config::Config;
use crate::env::Env;
use crate::routes;
use rume::app::{AppContext, Rume, init_tracing, shutdown_global_observability_async};
use rume::routing::health_routes;

#[derive(Clone)]
pub struct AppState;

pub fn build_router(
    ctx: AppContext<Env, Config, AppState>,
) -> rume::routing::Router<AppContext<Env, Config, AppState>> {
    let router = routes::router(ctx.clone());
    health_routes::<Env, Config, AppState>().merge(router)
}

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let env = rume::env::load::<Env>()?;
    let config = rume::config::load::<Config>()?;

    println!("starting {}...", config.app.name);

    // Application context: handlers access env/config/state via `State<AppContext<Env, Config, AppState>>`.
    let ctx = AppContext::builder(env.clone(), config)
        .state(AppState)
        .build();

    let router = build_router(ctx.clone());

    let run_result = Rume::builder(ctx)
        .router(router)
        .bind((env.host, env.port))
        .run()
        .await;

    shutdown_global_observability_async().await;
    run_result?;

    Ok(())
}
