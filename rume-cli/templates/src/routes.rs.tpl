use rume::prelude::*;

pub fn router() -> Router<()> {
    Router::new().route("/health", routing::get(crate::handlers::health::health))
}
