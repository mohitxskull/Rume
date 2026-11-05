use crate::state::WorkbenchState;
use axum::Json;
use axum::extract::State;
use serde::Serialize;

#[derive(Serialize)]
pub struct Health {
    pub status: &'static str,
    pub leased_connections: u32,
    pub in_flight_jobs: u32,
}

pub async fn health(State(state): State<WorkbenchState>) -> Json<Health> {
    let pool_status = state.pool.status();
    let in_flight = state
        .worker
        .as_ref()
        .map(|w| w.in_flight_count())
        .unwrap_or(0);
    Json(Health {
        status: "ok",
        leased_connections: pool_status.leased_connections,
        in_flight_jobs: in_flight,
    })
}
