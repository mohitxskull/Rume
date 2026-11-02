use crate::config::Config;
use crate::env::Env;
use crate::routes;
use rume::app::{AppContext, Environment, ProjectPaths, Rume, Availability, init_tracing};
use rume::routing::health_routes;

pub struct AppState;

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let env = rume::env::load::<Env>()?;
    let config = rume::config::load::<Config>()?;
    let paths = ProjectPaths::from_current_dir();
    let availability = Availability::new();
    let health_checks = rume::routing::HealthChecks::new();
    let app_state = AppState;

    println!("starting {}...", config.app.name);

    // Application context: handlers access env/config/state via `State<AppContext<Env, Config, AppState>>`.
    let ctx = AppContext::new(env.clone(), config.clone(), paths, availability.clone(), health_checks, app_state);

    let router = routes::router(ctx.clone());
    let router = health_routes::<Env, Config, AppState>().merge(router);

    Rume::builder(ctx)
        .router(router)
        .bind((env.host, env.port))
        .run()
        .await?;

    Ok(())
}
