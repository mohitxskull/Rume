use crate::app::AppState;
use crate::config::Config;
use crate::env::Env;
use rume::app::AppContext;
use rume::prelude::*;

pub fn router(
    _ctx: AppContext<Env, Config, AppState>,
) -> Router<AppContext<Env, Config, AppState>> {
    let mut router = Router::<AppContext<Env, Config, AppState>>::new();
    router = router.get("/health", crate::handlers::health::health);
    // rume:routes
    router
}
