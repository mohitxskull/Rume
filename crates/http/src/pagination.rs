//! OpenAPI-aware pagination extractors and response wrappers.

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{FromRequestParts, Query};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use rume_data::persistence::pagination::{
    CursorPage, CursorPageRequest, DEFAULT_MAX_PER_PAGE, DEFAULT_PAGE, DEFAULT_PER_PAGE,
    OffsetPage, OffsetPageRequest,
};
use rume_foundation::error::{AppError, ValidationErrorDetail};

use crate::openapi::spec::{HasOpenApiSchema, OpenApiSchema};

#[derive(Debug, Deserialize)]
struct RawOffsetPageQuery {
    page: Option<u64>,
    per_page: Option<u64>,
}

/// Axum query extractor for bounded offset pagination (`?page=1&per_page=20`).
///
/// Rejects malformed query parameters with [`AppError::BadRequest`] (HTTP 400) and
/// out-of-bounds values (`page < 1`, `per_page < 1`, or `per_page > 100`) with
/// [`AppError::Validation`] (HTTP 422).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OffsetPageQuery {
    /// 1-indexed page number.
    pub page: u64,
    /// Items per page (`1..=100`).
    pub per_page: u64,
}

impl Default for OffsetPageQuery {
    fn default() -> Self {
        Self {
            page: DEFAULT_PAGE,
            per_page: DEFAULT_PER_PAGE,
        }
    }
}

impl OffsetPageQuery {
    /// Converts this validated HTTP query into a [`OffsetPageRequest`].
    pub fn into_request(self) -> OffsetPageRequest {
        OffsetPageRequest::new(self.page, self.per_page)
    }
}

impl From<OffsetPageQuery> for OffsetPageRequest {
    fn from(q: OffsetPageQuery) -> Self {
        q.into_request()
    }
}

impl<S> FromRequestParts<S> for OffsetPageQuery
where
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Query(raw) = Query::<RawOffsetPageQuery>::from_request_parts(parts, state)
            .await
            .map_err(|err: QueryRejection| AppError::BadRequest {
                code: "BAD_REQUEST",
                message: err.to_string(),
            })?;

        let page = raw.page.unwrap_or(DEFAULT_PAGE);
        let per_page = raw.per_page.unwrap_or(DEFAULT_PER_PAGE);

        let mut details = Vec::new();
        if page < 1 {
            details.push(ValidationErrorDetail {
                field: "page".to_string(),
                message: "page must be at least 1".to_string(),
            });
        }
        if !(1..=DEFAULT_MAX_PER_PAGE).contains(&per_page) {
            details.push(ValidationErrorDetail {
                field: "per_page".to_string(),
                message: format!("per_page must be between 1 and {DEFAULT_MAX_PER_PAGE}"),
            });
        }
        if !details.is_empty() {
            return Err(AppError::Validation { details });
        }

        Ok(Self { page, per_page })
    }
}

impl HasOpenApiSchema for OffsetPageQuery {
    fn schema() -> OpenApiSchema {
        OpenApiSchema::object()
            .with_property("page", OpenApiSchema::integer().with_minimum(1))
            .with_property(
                "per_page",
                OpenApiSchema::integer()
                    .with_minimum(1)
                    .with_maximum(DEFAULT_MAX_PER_PAGE as i64),
            )
    }
}

#[derive(Debug, Deserialize)]
struct RawCursorPageQuery {
    cursor: Option<String>,
    limit: Option<u64>,
}

/// Axum query extractor for bounded cursor pagination (`?cursor=...&limit=20`).
///
/// Rejects malformed query parameters with [`AppError::BadRequest`] (HTTP 400) and
/// invalid bounds (`limit < 1`, `limit > 100`, or empty `cursor`) with
/// [`AppError::Validation`] (HTTP 422).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorPageQuery {
    /// Optional opaque or signed cursor token.
    pub cursor: Option<String>,
    /// Maximum items to return (`1..=100`).
    pub limit: u64,
}

impl Default for CursorPageQuery {
    fn default() -> Self {
        Self {
            cursor: None,
            limit: DEFAULT_PER_PAGE,
        }
    }
}

impl CursorPageQuery {
    /// Converts this validated HTTP query into a [`CursorPageRequest`].
    pub fn into_request(self) -> CursorPageRequest {
        CursorPageRequest::new(self.cursor, self.limit)
    }
}

impl From<CursorPageQuery> for CursorPageRequest {
    fn from(q: CursorPageQuery) -> Self {
        q.into_request()
    }
}

impl<S> FromRequestParts<S> for CursorPageQuery
where
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Query(raw) = Query::<RawCursorPageQuery>::from_request_parts(parts, state)
            .await
            .map_err(|err: QueryRejection| AppError::BadRequest {
                code: "BAD_REQUEST",
                message: err.to_string(),
            })?;

        let limit = raw.limit.unwrap_or(DEFAULT_PER_PAGE);
        let mut details = Vec::new();
        if let Some(ref cursor) = raw.cursor
            && cursor.is_empty()
        {
            details.push(ValidationErrorDetail {
                field: "cursor".to_string(),
                message: "cursor must not be empty".to_string(),
            });
        }
        if !(1..=DEFAULT_MAX_PER_PAGE).contains(&limit) {
            details.push(ValidationErrorDetail {
                field: "limit".to_string(),
                message: format!("limit must be between 1 and {DEFAULT_MAX_PER_PAGE}"),
            });
        }
        if !details.is_empty() {
            return Err(AppError::Validation { details });
        }

        Ok(Self {
            cursor: raw.cursor,
            limit,
        })
    }
}

impl HasOpenApiSchema for CursorPageQuery {
    fn schema() -> OpenApiSchema {
        OpenApiSchema::object()
            .with_property("cursor", OpenApiSchema::string().with_min_length(1))
            .with_property(
                "limit",
                OpenApiSchema::integer()
                    .with_minimum(1)
                    .with_maximum(DEFAULT_MAX_PER_PAGE as i64),
            )
    }
}

/// JSON response wrapper for offset-paginated results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaginatedResponse<T> {
    /// Items on the current page.
    pub items: Vec<T>,
    /// Total matching items across all pages.
    pub total: u64,
    /// Current 1-indexed page number.
    pub page: u64,
    /// Items per page.
    pub per_page: u64,
    /// Total number of pages.
    pub total_pages: u64,
    /// Whether a next page exists.
    pub has_next: bool,
    /// Whether a previous page exists.
    pub has_prev: bool,
}

impl<T> From<OffsetPage<T>> for PaginatedResponse<T> {
    fn from(page: OffsetPage<T>) -> Self {
        Self {
            items: page.items,
            total: page.total,
            page: page.page,
            per_page: page.per_page,
            total_pages: page.total_pages,
            has_next: page.has_next,
            has_prev: page.has_prev,
        }
    }
}

impl<T: HasOpenApiSchema> HasOpenApiSchema for PaginatedResponse<T> {
    fn schema() -> OpenApiSchema {
        OpenApiSchema::object()
            .with_required_property("items", OpenApiSchema::array(T::schema()))
            .with_required_property("total", u64::schema())
            .with_required_property("page", u64::schema().with_minimum(1))
            .with_required_property("per_page", u64::schema().with_minimum(1))
            .with_required_property("total_pages", u64::schema())
            .with_required_property("has_next", bool::schema())
            .with_required_property("has_prev", bool::schema())
    }
}

impl<T: Serialize> IntoResponse for PaginatedResponse<T> {
    fn into_response(self) -> Response {
        Json(self).into_response()
    }
}

/// JSON response wrapper for cursor-paginated results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorPaginatedResponse<T> {
    /// Items on the current page.
    pub items: Vec<T>,
    /// Cursor token for the next page, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Cursor token for the previous page, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev_cursor: Option<String>,
    /// Whether additional items exist beyond this page.
    pub has_next: bool,
}

impl<T> From<CursorPage<T>> for CursorPaginatedResponse<T> {
    fn from(page: CursorPage<T>) -> Self {
        Self {
            items: page.items,
            next_cursor: page.next_cursor,
            prev_cursor: page.prev_cursor,
            has_next: page.has_next,
        }
    }
}

impl<T: HasOpenApiSchema> HasOpenApiSchema for CursorPaginatedResponse<T> {
    fn schema() -> OpenApiSchema {
        OpenApiSchema::object()
            .with_required_property("items", OpenApiSchema::array(T::schema()))
            .with_property("next_cursor", OpenApiSchema::string())
            .with_property("prev_cursor", OpenApiSchema::string())
            .with_required_property("has_next", bool::schema())
    }
}

impl<T: Serialize> IntoResponse for CursorPaginatedResponse<T> {
    fn into_response(self) -> Response {
        Json(self).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openapi::spec::HasOpenApiSchema;
    use axum::extract::FromRequestParts;
    use axum::http::Request;
    use rume_data::persistence::pagination::OffsetPage;
    use rume_foundation::error::AppError;
    use serde::Serialize;

    #[tokio::test]
    async fn offset_page_query_extractor_parses_and_rejects_invalid_params() {
        let req = Request::builder()
            .uri("/items?page=2&per_page=25")
            .body(())
            .unwrap();
        let (mut parts, ()) = req.into_parts();
        let extracted = OffsetPageQuery::from_request_parts(&mut parts, &())
            .await
            .expect("valid query");
        assert_eq!(extracted.page, 2);
        assert_eq!(extracted.per_page, 25);

        // Invalid syntax -> 400 BadRequest
        let bad_req = Request::builder().uri("/items?page=abc").body(()).unwrap();
        let (mut bad_parts, ()) = bad_req.into_parts();
        let err = OffsetPageQuery::from_request_parts(&mut bad_parts, &())
            .await
            .expect_err("should reject non-numeric page");
        assert_eq!(err.status_code().as_u16(), 400);
        assert!(matches!(err, AppError::BadRequest { .. }));

        // Out of bounds -> 422 Validation
        let oob_req = Request::builder()
            .uri("/items?page=0&per_page=101")
            .body(())
            .unwrap();
        let (mut oob_parts, ()) = oob_req.into_parts();
        let err = OffsetPageQuery::from_request_parts(&mut oob_parts, &())
            .await
            .expect_err("should reject out-of-bounds params");
        assert_eq!(err.status_code().as_u16(), 422);
        assert!(matches!(err, AppError::Validation { .. }));
    }

    #[tokio::test]
    async fn cursor_page_query_extractor_parses_and_rejects_invalid_params() {
        let req = Request::builder()
            .uri("/items?cursor=abc123&limit=15")
            .body(())
            .unwrap();
        let (mut parts, ()) = req.into_parts();
        let extracted = CursorPageQuery::from_request_parts(&mut parts, &())
            .await
            .expect("valid cursor query");
        assert_eq!(extracted.cursor.as_deref(), Some("abc123"));
        assert_eq!(extracted.limit, 15);

        let empty_req = Request::builder()
            .uri("/items?cursor=&limit=0")
            .body(())
            .unwrap();
        let (mut empty_parts, ()) = empty_req.into_parts();
        let err = CursorPageQuery::from_request_parts(&mut empty_parts, &())
            .await
            .expect_err("should reject empty cursor and zero limit");
        assert_eq!(err.status_code().as_u16(), 422);
    }

    #[derive(Serialize)]
    struct ItemDto {
        id: String,
    }

    impl HasOpenApiSchema for ItemDto {
        fn schema() -> OpenApiSchema {
            OpenApiSchema::object().with_required_property("id", OpenApiSchema::string())
        }
    }

    #[test]
    fn paginated_response_generates_valid_openapi_schema() {
        let schema = PaginatedResponse::<ItemDto>::schema();
        assert_eq!(schema.r#type, "object");
        let props = schema.properties.expect("properties");
        assert_eq!(props.get("items").expect("items").r#type, "array");
        assert_eq!(props.get("total").expect("total").r#type, "integer");
        assert_eq!(props.get("page").expect("page").r#type, "integer");
        assert_eq!(props.get("per_page").expect("per_page").r#type, "integer");
        assert_eq!(
            props.get("total_pages").expect("total_pages").r#type,
            "integer"
        );
        assert_eq!(props.get("has_next").expect("has_next").r#type, "boolean");
        assert_eq!(props.get("has_prev").expect("has_prev").r#type, "boolean");

        let page = OffsetPage {
            items: vec![ItemDto { id: "1".into() }],
            total: 1,
            page: 1,
            per_page: 20,
            total_pages: 1,
            has_next: false,
            has_prev: false,
        };
        let resp = PaginatedResponse::from(page);
        assert_eq!(resp.total, 1);
        assert_eq!(resp.items.len(), 1);
    }

    #[test]
    fn cursor_paginated_response_generates_valid_openapi_schema() {
        let schema = CursorPaginatedResponse::<ItemDto>::schema();
        assert_eq!(schema.r#type, "object");
        let props = schema.properties.expect("properties");
        assert_eq!(props.get("items").expect("items").r#type, "array");
        assert_eq!(
            props.get("next_cursor").expect("next_cursor").r#type,
            "string"
        );
        assert_eq!(
            props.get("prev_cursor").expect("prev_cursor").r#type,
            "string"
        );
        assert_eq!(props.get("has_next").expect("has_next").r#type, "boolean");
    }
}
