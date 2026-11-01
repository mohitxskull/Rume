use crate::config::Config;
use crate::env::Env;
use crate::routes;
use rume::app::{AppContext, Environment, ProjectPaths, Rume, Availability, init_tracing};

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let env = rume::env::load::<Env>()?;
    let config = rume::config::load::<Config>()?;
    let paths = ProjectPaths::from_current_dir();
    let availability = Availability::new();

    println!("starting {}...", config.app.name);

    // Application context: handlers access env/config via `State<AppContext<Env, Config>>`.
    let ctx = AppContext::new(env.clone(), config.clone(), paths, availability.clone());

    let router = routes::router(ctx.clone());

    Rume::builder(ctx)
        .router(router)
        .bind((env.host, env.port))
        .run()
        .await?;

    Ok(())
}
