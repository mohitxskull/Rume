use crate::config::Config;
use crate::env::Env;
use crate::routes;
use rume::prelude::*;

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let env = env::load::<Env>()?;
    let config = config::load::<Config>()?;

    println!("starting {}...", config.app.name);

    let router = routes::router();

    Rume::builder()
        .router(router)
        .bind((env.host, env.port))
        .run()
        .await?;

    Ok(())
}
