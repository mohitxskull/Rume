use crate::config::Config;
use crate::env::Env;
use rume::app::{AppContext, health_router};
use rume::prelude::*;

pub fn router(ctx: AppContext<Env, Config>) -> Router<AppContext<Env, Config>> {
    let mut router = health_router(ctx.availability.clone());
    router = router.route(
        "/users",
        rume::axum::routing::post(crate::handlers::users::create_user),
    );
    router = router.route("/posts", rume::axum::routing::get(crate::handlers::posts::index));
    router = router.with_state(ctx);
    // rume:routes
    router
}
