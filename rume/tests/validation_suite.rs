use axum::Json;
use axum::http::StatusCode;
use axum::routing::post;
use garde::Validate;
use rume::error::AppError;
use rume::prelude::*;
use serde::Deserialize;

#[derive(Debug, Deserialize, Validate)]
struct OrgPathParam {
    #[garde(length(min = 2, max = 20))]
    org: String,
}

#[derive(Debug, Deserialize, Validate)]
struct PaginationQuery {
    #[garde(range(min = 1, max = 50))]
    page: u32,
    #[garde(range(min = 1, max = 100))]
    per_page: u32,
}

#[derive(Debug, Deserialize, Validate)]
struct CreateMemberRequest {
    #[garde(email)]
    email: String,
    #[garde(length(min = 2, max = 50))]
    role: String,
}

#[derive(Debug, Deserialize, Validate)]
struct UpdateProfileForm {
    #[garde(length(min = 2, max = 30))]
    display_name: String,
    #[garde(url)]
    website: String,
}

async fn handle_multi_extractor(
    ValidatedPath(org): ValidatedPath<OrgPathParam>,
    ValidatedQuery(pagination): ValidatedQuery<PaginationQuery>,
    ValidatedJson(payload): ValidatedJson<CreateMemberRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    Ok(Json(serde_json::json!({
        "org": org.org,
        "page": pagination.page,
        "per_page": pagination.per_page,
        "email": payload.email,
        "role": payload.role,
    })))
}

async fn handle_path_and_form(
    ValidatedPath(org): ValidatedPath<OrgPathParam>,
    ValidatedForm(form): ValidatedForm<UpdateProfileForm>,
) -> Result<Json<serde_json::Value>, AppError> {
    Ok(Json(serde_json::json!({
        "org": org.org,
        "display_name": form.display_name,
        "website": form.website,
    })))
}

#[tokio::test]
async fn multi_extractor_success() {
    let router = rume::routing::Router::new()
        .route("/orgs/{org}/members", post(handle_multi_extractor))
        .route("/orgs/{org}/profile", post(handle_path_and_form));

    let app = TestApp::new(router);

    let body = r#"{"email":"admin@acme.corp","role":"owner"}"#;
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/orgs/acme/members?page=1&per_page=20")
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(body))
        .unwrap();

    let res = app.request(req).await;
    assert_eq!(res.status(), StatusCode::OK);

    let text = TestApp::body_text(res).await;
    let val: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(val["org"], "acme");
    assert_eq!(val["page"], 1);
    assert_eq!(val["per_page"], 20);
    assert_eq!(val["email"], "admin@acme.corp");
    assert_eq!(val["role"], "owner");
}

#[tokio::test]
async fn multi_extractor_rejections_are_problem_details() {
    let router =
        rume::routing::Router::new().route("/orgs/{org}/members", post(handle_multi_extractor));

    let app = TestApp::new(router);

    // 1. Path param validation failure -> 422
    let body = r#"{"email":"admin@acme.corp","role":"owner"}"#;
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/orgs/a/members?page=1&per_page=20")
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(body))
        .unwrap();

    let res = app.request(req).await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let val: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(val["code"], "VALIDATION_FAILED");
    assert_eq!(val["details"][0]["field"], "org");

    // 2. Query param validation failure -> 422
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/orgs/acme/members?page=999&per_page=20")
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(body))
        .unwrap();

    let res = app.request(req).await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let val: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(val["code"], "VALIDATION_FAILED");
    assert_eq!(val["details"][0]["field"], "page");

    // 3. Json body validation failure -> 422
    let bad_body = r#"{"email":"not-email","role":"o"}"#;
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/orgs/acme/members?page=1&per_page=20")
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(bad_body))
        .unwrap();

    let res = app.request(req).await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let val: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(val["code"], "VALIDATION_FAILED");
    let fields: Vec<&str> = val["details"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["field"].as_str().unwrap())
        .collect();
    assert!(fields.contains(&"email"));
    assert!(fields.contains(&"role"));
}

#[tokio::test]
async fn path_and_form_extractor_success_and_failure() {
    let router =
        rume::routing::Router::new().route("/orgs/{org}/profile", post(handle_path_and_form));

    let app = TestApp::new(router);

    // Success
    let form_body = "display_name=Acme+Corp&website=https%3A%2F%2Facme.corp";
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/orgs/acme/profile")
        .header(
            axum::http::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(axum::body::Body::from(form_body))
        .unwrap();

    let res = app.request(req).await;
    assert_eq!(res.status(), StatusCode::OK);

    // Validation failure on URL
    let bad_form = "display_name=Acme+Corp&website=not_a_valid_url";
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/orgs/acme/profile")
        .header(
            axum::http::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(axum::body::Body::from(bad_form))
        .unwrap();

    let res = app.request(req).await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let val: serde_json::Value = serde_json::from_str(&TestApp::body_text(res).await).unwrap();
    assert_eq!(val["code"], "VALIDATION_FAILED");
    assert_eq!(val["details"][0]["field"], "website");
}
