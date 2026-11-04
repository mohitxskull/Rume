//! Scenario 1: A small API with routing, validation, configuration, and persistence.
//!
//! Proves that Rume stays simple and lightweight for standard REST APIs without
//! requiring modular monolith ceremony or enterprise infrastructure.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use garde::Validate;
use rume::persistence::migration::Migrator;
use rume::persistence::pool::DatabasePool;
use rume::persistence::row::{DbRow, FromRow};
use rume::persistence::sqlite::SqliteBackend;
use rume::persistence::PersistenceError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub app_name: String,
    pub port: u16,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            app_name: "simple-api".into(),
            port: 8080,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub price_cents: u64,
    pub created_at: i64,
}

impl FromRow for Item {
    fn from_row(row: &DbRow) -> Result<Self, PersistenceError> {
        let price_int: i64 = row.get("price_cents")?;
        Ok(Self {
            id: row.get("id")?,
            title: row.get("title")?,
            description: row.get_opt("description")?,
            price_cents: price_int.max(0) as u64,
            created_at: row.get("created_at")?,
        })
    }
}

#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct CreateItemRequest {
    #[garde(length(min = 3, max = 100))]
    pub title: String,
    #[garde(skip)]
    pub description: Option<String>,
    #[garde(range(min = 1, max = 1_000_000))]
    pub price_cents: u64,
}

pub fn migrator() -> Migrator {
    rume::embed_migrations!("migrations").with_rume_core("sqlite")
}

pub fn build_app(pool: DatabasePool<SqliteBackend>) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/items", post(create_item).get(list_items))
        .route("/items/{id}", get(get_item).delete(delete_item))
        .with_state(pool)
}

async fn create_item(
    State(pool): State<DatabasePool<SqliteBackend>>,
    Json(payload): Json<CreateItemRequest>,
) -> Result<(StatusCode, Json<Item>), (StatusCode, String)> {
    if let Err(e) = payload.validate() {
        return Err((StatusCode::UNPROCESSABLE_ENTITY, e.to_string()));
    }

    let item = Item {
        id: format!("item_{}", uuid::Uuid::new_v4().simple()),
        title: payload.title,
        description: payload.description,
        price_cents: payload.price_cents,
        created_at: chrono_now_millis(),
    };

    pool.execute_params(
        "INSERT INTO items (id, title, description, price_cents, created_at) VALUES ($1, $2, $3, $4, $5)",
        &rume::params![
            &item.id,
            &item.title,
            &item.description,
            item.price_cents as i64,
            item.created_at,
        ],
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok((StatusCode::CREATED, Json(item)))
}

async fn get_item(
    State(pool): State<DatabasePool<SqliteBackend>>,
    Path(id): Path<String>,
) -> Result<Json<Item>, StatusCode> {
    let item: Option<Item> = pool
        .fetch_optional_as(
            "SELECT id, title, description, price_cents, created_at FROM items WHERE id = $1",
            &rume::params![&id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    item.map(Json).ok_or(StatusCode::NOT_FOUND)
}

async fn list_items(
    State(pool): State<DatabasePool<SqliteBackend>>,
) -> Result<Json<Vec<Item>>, (StatusCode, String)> {
    let items: Vec<Item> = pool
        .fetch_all_as(
            "SELECT id, title, description, price_cents, created_at FROM items ORDER BY created_at ASC",
            &[],
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(items))
}

async fn delete_item(
    State(pool): State<DatabasePool<SqliteBackend>>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let rows = pool
        .execute_params("DELETE FROM items WHERE id = $1", &rume::params![&id])
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if rows > 0 {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

fn chrono_now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
