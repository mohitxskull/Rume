use crate::{config, env, routes};
use rume::app::{Environment, ProjectPaths, Rume};

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let env = env::load()?;
    let config = config::load()?;
    let paths = ProjectPaths::from_current_dir();

    println!("starting {}...", config.app.name);

    let router = routes::router();

    Rume::builder()
        .environment(Environment::from_env())
        .paths(paths)
        .router(router)
        .bind((env.host, env.port))
        .run()
        .await?;

    Ok(())
}
