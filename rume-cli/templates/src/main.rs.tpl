use rume::app::{Availability, Environment, ProjectPaths, Rume};
use rume::axum::{Json, Router, routing::get};
use rume::config;
use rume::env;
use rume::serde::Serialize;

#[derive(rume::serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
struct Env {
    host: std::net::IpAddr,
    port: u16,
}

#[derive(rume::serde::Deserialize)]
struct Config {
    app: AppConfig,
}

#[derive(rume::serde::Deserialize)]
struct AppConfig {
    name: String,
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    app: String,
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let env = env::load::<Env>()
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    let config = config::load::<Config>()
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    let paths = ProjectPaths::from_current_dir();
    let availability = Availability::new();

    let availability_for_route = availability.clone();
    let app_name = config.app.name.clone();

    let router = Router::new().route(
        "/health",
        get(move || {
            let availability = availability_for_route.clone();
            let app_name = app_name.clone();
            async move {
                Json(Health {
                    status: if availability.is_ready() { "ok" } else { "starting" },
                    app: app_name,
                })
            }
        }),
    );

    Rume::builder()
        .environment(Environment::from_env())
        .paths(paths)
        .on_ready(|()| async move {
            // Startup tasks (migrations, warm-up, etc.) go here.
        })
        .router(router)
        .bind((env.host, env.port))
        .run()
        .await
}
