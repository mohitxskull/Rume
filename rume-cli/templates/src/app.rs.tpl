use crate::routes;
use crate::config::Config;
use crate::env::Env;
use rume::app::{Environment, ProjectPaths, Rume};

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let env = rume::env::load::<Env>()
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    let config = rume::config::load::<Config>()
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    let paths = ProjectPaths::from_current_dir();

    println!("starting {}...", config.app_name);

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
