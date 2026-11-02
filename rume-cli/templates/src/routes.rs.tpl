use crate::app::AppState;
use crate::config::Config;
use crate::env::Env;
use rume::app::AppContext;
use rume::prelude::*;

pub fn router(ctx: AppContext<Env, Config, AppState>) -> Router<AppContext<Env, Config, AppState>> {
    let mut router = Router::<AppContext<Env, Config, AppState>>::new();
    router = router.route(
        "/users",
        rume::axum::routing::get(crate::handlers::health::health),
    );
    router = router.route("/posts", rume::axum::routing::get(crate::handlers::health::health));
    // rume:routes
    router
}
