//! Milestone C end-to-end integration tests through the `rume` facade.
//!
//! Exercises:
//! 1. `#[derive(FromRow)]` + `DynamicInsert` / `DynamicSelect` + `OffsetPage` / HMAC `CursorPage` over HTTP
//! 2. `ReadWritePool` routing writes to primary, reads to replica, and RYOW fallback window
//! 3. `SoftDeletable` + `OptimisticLock` returning `AppError::Conflict` (409 Problem Details) on stale version
//! 4. `SeedRegistry` running dependent seeders idempotently on SQLite
//! 5. `ValidatedMultipart` Axum upload handler storing bytes in `LocalFsObjectStore`
//! 6. `S3ObjectStore` `put` / `get` / `head` / `delete` / `presign_get` against `wiremock`

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use rume::error::AppError;
use rume::pagination::{
    CursorPageQuery, CursorPaginatedResponse, OffsetPageQuery, PaginatedResponse,
};
use rume::persistence::{
    BoxFuture, CursorCodec, DatabaseConfig, DatabasePool, DatabaseTransaction, DynamicInsert,
    DynamicSelect, DynamicUpdate, PersistenceError, ReadWriteConfig, ReadWritePool, SeedRegistry,
    Seeder, SqliteBackend, execute_cas, soft_delete_update, where_not_deleted, with_version_check,
};
use rume::prelude::*;
use rume::storage::{LocalFsObjectStore, ObjectKey, ObjectStore, PutObjectOptions};
use rume::upload::ValidatedMultipart;
use sea_query::{Alias, Expr, ExprTrait, Order, Query};
use serde::{Deserialize, Serialize};
use tower::ServiceExt;

async fn setup_sqlite_pool(label: &str) -> DatabasePool<SqliteBackend> {
    let backend = SqliteBackend::in_memory()
        .await
        .expect("create in-memory sqlite backend");
    DatabasePool::new(
        backend,
        DatabaseConfig::new(format!("sqlite://memory/{label}")),
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, FromRow)]
struct ArticleRow {
    id: i64,
    title: String,
    version: i64,
    deleted_at: Option<String>,
}

impl SoftDeletable for ArticleRow {
    fn deleted_at(&self) -> Option<&str> {
        self.deleted_at.as_deref()
    }
}

impl OptimisticLock for ArticleRow {
    fn version(&self) -> i64 {
        self.version
    }
}

#[tokio::test]
async fn e2e_from_row_dynamic_query_and_pagination_over_http() {
    let pool = Arc::new(setup_sqlite_pool("pagination").await);
    pool.execute(
        "CREATE TABLE articles (
            id INTEGER PRIMARY KEY,
            title TEXT NOT NULL,
            version INTEGER NOT NULL DEFAULT 1,
            deleted_at TEXT NULL
        )",
    )
    .await
    .expect("create articles table");

    for idx in 1..=5_i64 {
        let mut insert = Query::insert();
        insert
            .into_table(Alias::new("articles"))
            .columns([Alias::new("id"), Alias::new("title"), Alias::new("version")])
            .values_panic([idx.into(), format!("Article {idx}").into(), 1_i64.into()]);
        DynamicInsert::new(insert)
            .build_for_pool(&pool)
            .expect("build insert")
            .execute(&pool)
            .await
            .expect("insert article");
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct ArticleCursor {
        after_id: i64,
    }

    let codec = Arc::new(CursorCodec::hmac_sha256(
        b"milestone-c-cursor-secret".to_vec(),
    ));

    let offset_router =
        Router::new()
            .route(
                "/articles/offset",
                get(
                    |State(pool): State<Arc<DatabasePool<SqliteBackend>>>,
                     query: OffsetPageQuery| async move {
                        let mut select = Query::select();
                        select
                            .columns([
                                Alias::new("id"),
                                Alias::new("title"),
                                Alias::new("version"),
                                Alias::new("deleted_at"),
                            ])
                            .from(Alias::new("articles"))
                            .order_by(Alias::new("id"), Order::Asc);
                        let page = DynamicSelect::new(select)
                            .paginate_offset::<SqliteBackend, ArticleRow>(
                                &pool,
                                &query.into_request(),
                            )
                            .await
                            .map_err(|e| AppError::internal(e.to_string()))?;
                        Ok::<_, AppError>(Json(PaginatedResponse::from(page)))
                    },
                ),
            )
            .with_state(Arc::clone(&pool));

    let cursor_router = Router::new()
        .route(
            "/articles/cursor",
            get(
                |State((pool, codec)): State<(
                    Arc<DatabasePool<SqliteBackend>>,
                    Arc<CursorCodec>,
                )>,
                 query: CursorPageQuery| async move {
                    let after_id = match query.cursor.as_deref() {
                        Some(token) => {
                            codec
                                .decode::<ArticleCursor>(token)
                                .map_err(|e| AppError::bad_request(e.to_string()))?
                                .after_id
                        }
                        None => 0,
                    };
                    let mut select = Query::select();
                    select
                        .columns([
                            Alias::new("id"),
                            Alias::new("title"),
                            Alias::new("version"),
                            Alias::new("deleted_at"),
                        ])
                        .from(Alias::new("articles"))
                        .and_where(Expr::col(Alias::new("id")).gt(after_id))
                        .order_by(Alias::new("id"), Order::Asc);
                    let page = DynamicSelect::new(select)
                        .paginate_cursor::<SqliteBackend, ArticleRow, _, _>(
                            &pool,
                            &query.clone().into_request(),
                            &codec,
                            |row| ArticleCursor { after_id: row.id },
                        )
                        .await
                        .map_err(|e| AppError::internal(e.to_string()))?;
                    Ok::<_, AppError>(Json(CursorPaginatedResponse::from(page)))
                },
            ),
        )
        .with_state((Arc::clone(&pool), Arc::clone(&codec)));

    let app = offset_router.merge(cursor_router);

    let offset_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/articles/offset?page=2&per_page=2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("offset request");
    assert_eq!(offset_resp.status(), StatusCode::OK);
    let offset_bytes = axum::body::to_bytes(offset_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let offset_page: PaginatedResponse<ArticleRow> = serde_json::from_slice(&offset_bytes).unwrap();
    assert_eq!(offset_page.total, 5);
    assert_eq!(offset_page.total_pages, 3);
    assert!(offset_page.has_prev);
    assert!(offset_page.has_next);
    assert_eq!(
        offset_page.items.iter().map(|a| a.id).collect::<Vec<_>>(),
        vec![3, 4]
    );

    let cursor_resp_1 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/articles/cursor?limit=2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("cursor page 1 request");
    assert_eq!(cursor_resp_1.status(), StatusCode::OK);
    let cursor_bytes_1 = axum::body::to_bytes(cursor_resp_1.into_body(), usize::MAX)
        .await
        .unwrap();
    let cursor_page_1: CursorPaginatedResponse<ArticleRow> =
        serde_json::from_slice(&cursor_bytes_1).unwrap();
    assert!(cursor_page_1.has_next);
    assert_eq!(
        cursor_page_1.items.iter().map(|a| a.id).collect::<Vec<_>>(),
        vec![1, 2]
    );
    let next_token = cursor_page_1.next_cursor.expect("expected next_cursor");

    let cursor_resp_2 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/articles/cursor?cursor={next_token}&limit=2"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("cursor page 2 request");
    assert_eq!(cursor_resp_2.status(), StatusCode::OK);
    let cursor_bytes_2 = axum::body::to_bytes(cursor_resp_2.into_body(), usize::MAX)
        .await
        .unwrap();
    let cursor_page_2: CursorPaginatedResponse<ArticleRow> =
        serde_json::from_slice(&cursor_bytes_2).unwrap();
    assert_eq!(
        cursor_page_2.items.iter().map(|a| a.id).collect::<Vec<_>>(),
        vec![3, 4]
    );

    // Tampered HMAC cursor is rejected with 400 Bad Request.
    let tampered_resp = app
        .oneshot(
            Request::builder()
                .uri("/articles/cursor?cursor=tampered.token&limit=2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("tampered cursor request");
    assert_eq!(tampered_resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn e2e_read_write_pool_routing_and_ryow_fallback() {
    let primary = setup_sqlite_pool("primary").await;
    let replica = setup_sqlite_pool("replica").await;

    primary
        .execute("CREATE TABLE markers (source TEXT NOT NULL)")
        .await
        .unwrap();
    primary
        .execute("INSERT INTO markers (source) VALUES ('primary')")
        .await
        .unwrap();

    replica
        .execute("CREATE TABLE markers (source TEXT NOT NULL)")
        .await
        .unwrap();
    replica
        .execute("INSERT INTO markers (source) VALUES ('replica')")
        .await
        .unwrap();

    let rw = ReadWritePool::new(
        primary,
        vec![replica],
        ReadWriteConfig::default().with_ryow_window(Duration::from_millis(80)),
    );

    // Before any write, reads route to the replica.
    let row_before = rw
        .fetch_one("SELECT source FROM markers", &[])
        .await
        .expect("read from replica before write");
    assert_eq!(row_before.get::<String>("source").unwrap(), "replica");

    // A write on ReadWritePool routes to primary and activates the RYOW window.
    rw.execute("INSERT INTO markers (source) VALUES ('primary-write')")
        .await
        .expect("write to primary");
    assert!(rw.is_within_ryow_window());

    let rows_during_ryow = rw
        .fetch_all("SELECT source FROM markers ORDER BY source", &[])
        .await
        .expect("read from primary during RYOW window");
    let sources: Vec<String> = rows_during_ryow
        .iter()
        .map(|r| r.get::<String>("source").unwrap())
        .collect();
    assert_eq!(sources, vec!["primary", "primary-write"]);

    // After the RYOW window expires, reads route back to the replica.
    tokio::time::pause();
    tokio::time::advance(Duration::from_millis(120)).await;
    tokio::time::resume();
    assert!(!rw.is_within_ryow_window());
    let row_after = rw
        .fetch_one("SELECT source FROM markers", &[])
        .await
        .expect("read from replica after RYOW window");
    assert_eq!(row_after.get::<String>("source").unwrap(), "replica");
}

#[tokio::test]
async fn e2e_soft_delete_and_optimistic_lock_returns_409_conflict() {
    let pool = Arc::new(setup_sqlite_pool("conventions").await);
    pool.execute(
        "CREATE TABLE documents (
            id INTEGER PRIMARY KEY,
            title TEXT NOT NULL,
            version INTEGER NOT NULL DEFAULT 1,
            deleted_at TEXT NULL
        )",
    )
    .await
    .unwrap();
    pool.execute("INSERT INTO documents (id, title, version) VALUES (1, 'Draft', 1)")
        .await
        .unwrap();
    pool.execute(
        "INSERT INTO documents (id, title, version, deleted_at) VALUES (2, 'Archived', 1, '2026-01-01T00:00:00Z')",
    )
    .await
    .unwrap();

    // Soft-deleted rows are excluded by `where_not_deleted`.
    let mut select = Query::select();
    select
        .columns([
            Alias::new("id"),
            Alias::new("title"),
            Alias::new("version"),
            Alias::new("deleted_at"),
        ])
        .from(Alias::new("documents"));
    let active_docs = where_not_deleted(DynamicSelect::new(select), Alias::new("deleted_at"))
        .build_for_pool(&pool)
        .unwrap()
        .fetch_all_as::<SqliteBackend, ArticleRow>(&pool)
        .await
        .unwrap();
    assert_eq!(active_docs.len(), 1);
    assert_eq!(active_docs[0].id, 1);
    assert!(!active_docs[0].is_deleted());
    assert_eq!(active_docs[0].version(), 1);

    #[derive(Deserialize)]
    struct UpdateDocRequest {
        title: String,
        expected_version: i64,
    }

    let app = Router::new()
        .route(
            "/documents/{id}",
            post(
                |State(pool): State<Arc<DatabasePool<SqliteBackend>>>,
                 Path(id): Path<i64>,
                 Json(body): Json<UpdateDocRequest>| async move {
                    let mut upd = Query::update();
                    upd.table(Alias::new("documents"))
                        .value(Alias::new("title"), body.title)
                        .and_where(Expr::col(Alias::new("id")).eq(id))
                        .and_where(Expr::col(Alias::new("deleted_at")).is_null());
                    let built = with_version_check(
                        DynamicUpdate::new(upd),
                        Alias::new("version"),
                        body.expected_version,
                    )
                    .build_for_pool(&pool)
                    .map_err(|e| AppError::internal(e.to_string()))?;

                    match execute_cas(&pool, &built, "Document", body.expected_version).await {
                        Ok(_) => Ok::<_, AppError>(StatusCode::OK),
                        Err(PersistenceError::VersionConflict {
                            entity, expected, ..
                        }) => Err(AppError::conflict(format!(
                            "{entity} version conflict (expected {expected})"
                        ))),
                        Err(e) => Err(AppError::internal(e.to_string())),
                    }
                },
            ),
        )
        .with_state(Arc::clone(&pool));

    // Valid CAS update succeeds and bumps version to 2.
    let ok_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/documents/1")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"title":"Published","expected_version":1}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ok_resp.status(), StatusCode::OK);

    // Stale version returns 409 Conflict with RFC 9457 Problem Details.
    let conflict_resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/documents/1")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"title":"Stale","expected_version":1}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(conflict_resp.status(), StatusCode::CONFLICT);
    let conflict_bytes = axum::body::to_bytes(conflict_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let problem: serde_json::Value = serde_json::from_slice(&conflict_bytes).unwrap();
    assert_eq!(problem["status"], 409);
    assert_eq!(problem["title"], "Conflict");

    // Soft-deleting document 1 marks `deleted_at` and excludes it from `where_not_deleted`.
    let mut del_upd = soft_delete_update(Alias::new("documents"), "2026-11-26T00:00:00Z");
    del_upd
        .as_inner_mut()
        .and_where(Expr::col(Alias::new("id")).eq(1_i64));
    let affected = del_upd
        .build_for_pool(&pool)
        .unwrap()
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(affected, 1);
}

#[tokio::test]
async fn e2e_seed_registry_idempotent_with_dependencies() {
    struct RolesSeeder;
    impl Seeder for RolesSeeder {
        fn name(&self) -> &'static str {
            "roles"
        }

        fn run<'a>(
            &'a self,
            tx: &'a dyn DatabaseTransaction,
        ) -> BoxFuture<'a, Result<(), PersistenceError>> {
            Box::pin(async move {
                tx.execute("INSERT INTO roles (id, name) VALUES (1, 'admin')")
                    .await?;
                Ok(())
            })
        }
    }

    struct AdminUserSeeder;
    impl Seeder for AdminUserSeeder {
        fn name(&self) -> &'static str {
            "admin_user"
        }

        fn dependencies(&self) -> &[&'static str] {
            &["roles"]
        }

        fn run<'a>(
            &'a self,
            tx: &'a dyn DatabaseTransaction,
        ) -> BoxFuture<'a, Result<(), PersistenceError>> {
            Box::pin(async move {
                tx.execute(
                    "INSERT INTO users (id, role_id, email) VALUES (1, 1, 'admin@rume.dev')",
                )
                .await?;
                Ok(())
            })
        }
    }

    let pool = setup_sqlite_pool("seeds").await;
    pool.execute("CREATE TABLE roles (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
        .await
        .unwrap();
    pool.execute(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, role_id INTEGER NOT NULL REFERENCES roles(id), email TEXT NOT NULL)",
    )
    .await
    .unwrap();

    // Register dependent seeder before its dependency to verify topological sorting.
    let mut registry = SeedRegistry::new();
    registry.register(AdminUserSeeder).register(RolesSeeder);

    let first_run = registry.run_all(&pool).await.expect("first seed run");
    assert_eq!(first_run.applied, vec!["roles", "admin_user"]);
    assert!(first_run.skipped.is_empty());

    // Second run is idempotent: both seeders are skipped.
    let second_run = registry.run_all(&pool).await.expect("second seed run");
    assert!(second_run.applied.is_empty());
    assert_eq!(second_run.skipped, vec!["roles", "admin_user"]);
}

#[tokio::test]
async fn e2e_multipart_upload_into_local_fs_object_store() {
    let dir = tempfile::tempdir().expect("tempdir for object store");
    let store = Arc::new(LocalFsObjectStore::new(dir.path()).expect("init LocalFsObjectStore"));

    let app =
        Router::new()
            .route(
                "/upload",
                post(
                    |State(store): State<Arc<LocalFsObjectStore>>,
                     multipart: ValidatedMultipart| async move {
                        let file = multipart
                            .files
                            .into_iter()
                            .next()
                            .ok_or_else(|| AppError::bad_request("missing file"))?;
                        let key = ObjectKey::new(format!("uploads/{}", file.sanitized_file_name))
                            .map_err(|e| AppError::bad_request(e.to_string()))?;
                        let meta = store
                            .put(
                                &key,
                                file.bytes,
                                PutObjectOptions {
                                    content_type: file.content_type,
                                },
                            )
                            .await
                            .map_err(|e| AppError::internal(e.to_string()))?;
                        Ok::<_, AppError>((
                            StatusCode::CREATED,
                            Json(serde_json::json!({
                                "key": meta.key,
                                "size": meta.size,
                            })),
                        ))
                    },
                ),
            )
            .with_state(Arc::clone(&store));

    let boundary = "----rume-e2e-boundary";
    let body = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"../hello.txt\"\r\n\
         Content-Type: text/plain\r\n\r\n\
         Hello from Rume upload!\r\n\
         --{boundary}--\r\n"
    );

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/upload")
                .header(
                    CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .expect("upload request");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&resp_bytes).unwrap();
    assert_eq!(json["key"], "uploads/hello.txt");
    assert_eq!(json["size"], 23);

    let key = ObjectKey::new("uploads/hello.txt").unwrap();
    let stored = store.get(&key).await.expect("read back uploaded object");
    assert_eq!(stored, b"Hello from Rume upload!");
}

#[cfg(feature = "s3")]
#[tokio::test]
async fn e2e_s3_object_store_with_wiremock_and_presigned_url() {
    use rume::storage::{S3Config, S3ObjectStore};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let store = S3ObjectStore::new(S3Config {
        endpoint: server.uri(),
        region: "us-east-1".to_string(),
        bucket: "rume-e2e-bucket".to_string(),
        access_key_id: "test-access-key".to_string(),
        secret_access_key: "test-secret-key".into(),
        session_token: None,
        path_style: true,
        ..Default::default()
    })
    .expect("construct S3ObjectStore");

    let key = ObjectKey::new("assets/logo.png").unwrap();
    let obj_path = "/rume-e2e-bucket/assets/logo.png";

    Mock::given(method("PUT"))
        .and(path(obj_path))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"abc123\""))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path(obj_path))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"png-bytes".to_vec()))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("HEAD"))
        .and(path(obj_path))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Length", "9")
                .insert_header("Content-Type", "image/png")
                .insert_header("ETag", "\"abc123\""),
        )
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("DELETE"))
        .and(path(obj_path))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let put_meta = store
        .put(
            &key,
            b"png-bytes".to_vec(),
            PutObjectOptions {
                content_type: Some("image/png".to_string()),
            },
        )
        .await
        .expect("S3 put");
    assert_eq!(put_meta.etag.as_deref(), Some("\"abc123\""));

    let fetched = store.get(&key).await.expect("S3 get");
    assert_eq!(fetched, b"png-bytes");

    let head_meta = store.head(&key).await.expect("S3 head");
    assert_eq!(head_meta.size, 9);
    assert_eq!(head_meta.content_type.as_deref(), Some("image/png"));

    let presigned = store.presign_get(&key, 300).await.expect("S3 presign_get");
    assert_eq!(presigned.method, "GET");
    assert_eq!(presigned.expires_in_secs, 300);
    assert!(presigned.url.contains("X-Amz-Signature="));

    store.delete(&key).await.expect("S3 delete");
}
