use crate::config::Config;
use crate::env::Env;
use rume::app::AppContext;
use rume::prelude::*;

pub fn router(ctx: AppContext<Env, Config>) -> Router<AppContext<Env, Config>> {
    let mut router = Router::<AppContext<Env, Config>>::new();
    router = router.route(
        "/users",
        rume::axum::routing::get(crate::handlers::health::health),
    );
    router = router.route("/posts", rume::axum::routing::get(crate::handlers::health::health));
    // rume:routes
    router
}
