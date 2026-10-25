use crate::config::Config;
use crate::env::Env;
use crate::routes;
use rume::app::{Environment, ProjectPaths, Rume, Availability, health_router, init_tracing};
use rume::prelude::*;

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let env = rume::env::load::<Env>()?;
    let config = rume::config::load::<Config>()?;
    let paths = ProjectPaths::from_current_dir();
    let availability = Availability::new();

    println!("starting {}...", config.app.name);

    let router = health_router(availability.clone()).merge(routes::router());

    Rume::builder()
        .environment(Environment::from_env())
        .paths(paths)
        .router(router)
        .bind((env.host, env.port))
        .run()
        .await?;

    Ok(())
}
