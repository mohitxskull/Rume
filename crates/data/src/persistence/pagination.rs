//! Offset and cursor-based pagination primitives.
//!
//! Supports bounded offset pagination ([`OffsetPageRequest`], [`OffsetPage`]) and
//! opaque or HMAC-SHA256 signed cursor pagination ([`CursorPageRequest`], [`CursorPage`],
//! [`CursorCodec`]), plus [`DynamicSelect`] execution helpers.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit, Mac};
use sea_query::{Alias, Asterisk, Expr, ExprTrait, Query};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::persistence::backend::DatabaseBackend;
use crate::persistence::builder::DynamicSelect;
use crate::persistence::error::PersistenceError;
use crate::persistence::pool::DatabasePool;
use crate::persistence::row::FromRow;

/// Default page number (1-indexed).
pub const DEFAULT_PAGE: u64 = 1;
/// Default number of items per page.
pub const DEFAULT_PER_PAGE: u64 = 20;
/// Default upper bound on items per page.
pub const DEFAULT_MAX_PER_PAGE: u64 = 100;

/// Bounded offset pagination request (`page >= 1`, `1 <= per_page <= max_per_page`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OffsetPageRequest {
    /// 1-indexed page number.
    pub page: u64,
    /// Number of items per page.
    pub per_page: u64,
}

impl Default for OffsetPageRequest {
    fn default() -> Self {
        Self {
            page: DEFAULT_PAGE,
            per_page: DEFAULT_PER_PAGE,
        }
    }
}

impl OffsetPageRequest {
    /// Creates a new [`OffsetPageRequest`], clamping `page >= 1` and `1 <= per_page <= DEFAULT_MAX_PER_PAGE`.
    pub fn new(page: u64, per_page: u64) -> Self {
        Self::new_with_max(page, per_page, DEFAULT_MAX_PER_PAGE)
    }

    /// Creates a new [`OffsetPageRequest`] with a custom `max_per_page` upper bound.
    pub fn new_with_max(page: u64, per_page: u64, max_per_page: u64) -> Self {
        let max_bound = std::cmp::max(max_per_page, 1);
        Self {
            page: std::cmp::max(page, 1),
            per_page: per_page.clamp(1, max_bound),
        }
    }

    /// Zero-based SQL `OFFSET` corresponding to this page request.
    pub fn offset(&self) -> u64 {
        self.page.saturating_sub(1).saturating_mul(self.per_page)
    }

    /// SQL `LIMIT` corresponding to this page request.
    pub fn limit(&self) -> u64 {
        self.per_page
    }
}

/// Result of an offset-paginated query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OffsetPage<T> {
    /// Items on the current page.
    pub items: Vec<T>,
    /// Total matching items across all pages.
    pub total: u64,
    /// Current 1-indexed page number.
    pub page: u64,
    /// Maximum items per page.
    pub per_page: u64,
    /// Total number of pages (`0` when `total == 0`).
    pub total_pages: u64,
    /// Whether a subsequent page exists.
    pub has_next: bool,
    /// Whether a preceding page exists.
    pub has_prev: bool,
}

impl<T> OffsetPage<T> {
    /// Constructs an [`OffsetPage`] from fetched `items`, total item count, and the originating request.
    pub fn new(items: Vec<T>, total: u64, req: &OffsetPageRequest) -> Self {
        let per_page = std::cmp::max(req.per_page, 1);
        let page = std::cmp::max(req.page, 1);
        let total_pages = if total == 0 {
            0
        } else {
            total.div_ceil(per_page)
        };
        let has_prev = page > 1 && total_pages > 0;
        let has_next = page < total_pages;

        Self {
            items,
            total,
            page,
            per_page,
            total_pages,
            has_next,
            has_prev,
        }
    }
}

/// Bounded cursor pagination request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorPageRequest {
    /// Opaque or signed cursor token pointing to the boundary of the previous page.
    pub cursor: Option<String>,
    /// Maximum number of items to return.
    pub limit: u64,
}

impl Default for CursorPageRequest {
    fn default() -> Self {
        Self {
            cursor: None,
            limit: DEFAULT_PER_PAGE,
        }
    }
}

impl CursorPageRequest {
    /// Creates a new [`CursorPageRequest`], clamping `1 <= limit <= DEFAULT_MAX_PER_PAGE`.
    pub fn new(cursor: Option<String>, limit: u64) -> Self {
        Self::new_with_max(cursor, limit, DEFAULT_MAX_PER_PAGE)
    }

    /// Creates a new [`CursorPageRequest`] with a custom `max_limit` upper bound.
    pub fn new_with_max(cursor: Option<String>, limit: u64, max_limit: u64) -> Self {
        let max_bound = std::cmp::max(max_limit, 1);
        Self {
            cursor: cursor.filter(|c| !c.is_empty()),
            limit: limit.clamp(1, max_bound),
        }
    }
}

/// Result of a cursor-paginated query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorPage<T> {
    /// Items on the current page.
    pub items: Vec<T>,
    /// Cursor token for fetching the next page, if `has_next` is `true`.
    pub next_cursor: Option<String>,
    /// Cursor token for navigating back to the previous boundary, when a cursor was supplied.
    pub prev_cursor: Option<String>,
    /// Whether additional items exist beyond this page.
    pub has_next: bool,
}

/// Encodes and decodes pagination cursors as URL-safe base64url JSON payloads,
/// optionally authenticated with HMAC-SHA256.
#[derive(Clone)]
pub enum CursorCodec {
    /// Unsigned URL-safe base64url JSON payload.
    Opaque,
    /// HMAC-SHA256 signed URL-safe base64url JSON payload (`<payload_b64>.<sig_b64>`).
    HmacSha256 { key: Vec<u8> },
}

impl std::fmt::Debug for CursorCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Opaque => f.write_str("Opaque"),
            Self::HmacSha256 { .. } => f
                .debug_struct("HmacSha256")
                .field("key", &"[REDACTED]")
                .finish(),
        }
    }
}

impl CursorCodec {
    /// Creates an unsigned opaque cursor codec.
    pub fn opaque() -> Self {
        Self::Opaque
    }

    /// Creates an HMAC-SHA256 authenticated cursor codec using `key`.
    pub fn hmac_sha256(key: impl Into<Vec<u8>>) -> Self {
        Self::HmacSha256 { key: key.into() }
    }

    /// Serializes `payload` into a cursor token string.
    pub fn encode<T: Serialize>(&self, payload: &T) -> Result<String, PersistenceError> {
        let json = serde_json::to_vec(payload)
            .map_err(|e| PersistenceError::QueryFailed(format!("failed to encode cursor: {e}")))?;
        let payload_b64 = URL_SAFE_NO_PAD.encode(json);

        match self {
            Self::Opaque => Ok(payload_b64),
            Self::HmacSha256 { key } => {
                let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|e| {
                    PersistenceError::QueryFailed(format!("invalid HMAC cursor key: {e}"))
                })?;
                mac.update(payload_b64.as_bytes());
                let sig_b64 = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
                Ok(format!("{payload_b64}.{sig_b64}"))
            }
        }
    }

    /// Decodes and (for [`CursorCodec::HmacSha256`]) constant-time verifies a cursor token.
    pub fn decode<T: DeserializeOwned>(&self, token: &str) -> Result<T, PersistenceError> {
        let payload_b64 = match self {
            Self::Opaque => token,
            Self::HmacSha256 { key } => {
                let (payload_part, sig_part) = token.split_once('.').ok_or_else(|| {
                    PersistenceError::QueryFailed("invalid signed cursor format".to_string())
                })?;
                let provided_sig = URL_SAFE_NO_PAD.decode(sig_part).map_err(|e| {
                    PersistenceError::QueryFailed(format!("invalid cursor signature encoding: {e}"))
                })?;

                let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|e| {
                    PersistenceError::QueryFailed(format!("invalid HMAC cursor key: {e}"))
                })?;
                mac.update(payload_part.as_bytes());
                let expected_sig = mac.finalize().into_bytes();

                let is_valid = provided_sig.len() == expected_sig.len()
                    && bool::from(provided_sig.as_slice().ct_eq(expected_sig.as_slice()));
                if !is_valid {
                    return Err(PersistenceError::QueryFailed(
                        "cursor signature verification failed".to_string(),
                    ));
                }
                payload_part
            }
        };

        let raw_json = URL_SAFE_NO_PAD.decode(payload_b64).map_err(|e| {
            PersistenceError::QueryFailed(format!("invalid cursor payload encoding: {e}"))
        })?;
        serde_json::from_slice(&raw_json)
            .map_err(|e| PersistenceError::QueryFailed(format!("invalid cursor JSON payload: {e}")))
    }
}

impl DynamicSelect {
    /// Executes an offset-paginated query, running a `COUNT(*)` subquery and a `LIMIT`/`OFFSET` fetch.
    pub async fn paginate_offset<B: DatabaseBackend, R: FromRow>(
        &self,
        pool: &DatabasePool<B>,
        req: &OffsetPageRequest,
    ) -> Result<OffsetPage<R>, PersistenceError> {
        let mut count_stmt = Query::select();
        count_stmt
            .expr(Expr::col(Asterisk).count())
            .from_subquery(self.as_inner().clone(), Alias::new("_rume_count"));
        let count_row = DynamicSelect::new(count_stmt)
            .build_for_pool(pool)?
            .fetch_one(pool)
            .await?;
        let total_i64: i64 = count_row.get_by_index(0)?;
        let total = u64::try_from(total_i64).unwrap_or(0);

        let mut items_stmt = self.as_inner().clone();
        items_stmt.limit(req.limit()).offset(req.offset());
        let items = DynamicSelect::new(items_stmt)
            .build_for_pool(pool)?
            .fetch_all_as::<B, R>(pool)
            .await?;

        Ok(OffsetPage::new(items, total, req))
    }

    /// Executes a cursor-paginated query by fetching `limit + 1` rows to detect `has_next`
    /// and encoding `next_cursor` / `prev_cursor` via `codec`.
    pub async fn paginate_cursor<B, R, C, F>(
        &self,
        pool: &DatabasePool<B>,
        req: &CursorPageRequest,
        codec: &CursorCodec,
        cursor_of: F,
    ) -> Result<CursorPage<R>, PersistenceError>
    where
        B: DatabaseBackend,
        R: FromRow,
        C: Serialize,
        F: Fn(&R) -> C,
    {
        let mut items_stmt = self.as_inner().clone();
        items_stmt.limit(req.limit.saturating_add(1));

        let mut rows = DynamicSelect::new(items_stmt)
            .build_for_pool(pool)?
            .fetch_all_as::<B, R>(pool)
            .await?;

        let has_next = (rows.len() as u64) > req.limit;
        if has_next {
            rows.truncate(req.limit as usize);
        }

        let next_cursor = if has_next {
            rows.last()
                .map(|row| codec.encode(&cursor_of(row)))
                .transpose()?
        } else {
            None
        };

        let prev_cursor = if req.cursor.is_some() {
            rows.first()
                .map(|row| codec.encode(&cursor_of(row)))
                .transpose()?
        } else {
            None
        };

        Ok(CursorPage {
            items: rows,
            next_cursor,
            prev_cursor,
            has_next,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::{DatabaseConfig, DbRow, SqliteBackend};
    use sea_query::{Order, Query};
    use serde::{Deserialize, Serialize};

    #[test]
    fn offset_page_computes_totals_and_clamps_bounds() {
        let clamped = OffsetPageRequest::new(0, 500);
        assert_eq!(clamped.page, 1);
        assert_eq!(clamped.per_page, 100);
        assert_eq!(clamped.offset(), 0);
        assert_eq!(clamped.limit(), 100);

        let req = OffsetPageRequest::new(2, 10);
        assert_eq!(req.offset(), 10);
        assert_eq!(req.limit(), 10);

        let page = OffsetPage::new(vec!["a", "b"], 25, &req);
        assert_eq!(page.total, 25);
        assert_eq!(page.page, 2);
        assert_eq!(page.per_page, 10);
        assert_eq!(page.total_pages, 3);
        assert!(page.has_next);
        assert!(page.has_prev);

        let last_req = OffsetPageRequest::new(3, 10);
        let last_page = OffsetPage::new(vec!["z"], 25, &last_req);
        assert!(!last_page.has_next);
        assert!(last_page.has_prev);

        let empty_page = OffsetPage::<String>::new(vec![], 0, &OffsetPageRequest::new(1, 10));
        assert_eq!(empty_page.total_pages, 0);
        assert!(!empty_page.has_next);
        assert!(!empty_page.has_prev);
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct TestCursor {
        id: i64,
    }

    #[test]
    fn hmac_cursor_codec_roundtrips_and_rejects_tampering() {
        let codec = CursorCodec::hmac_sha256(b"super-secret-signing-key-32bytes".to_vec());
        let cursor = TestCursor { id: 42 };
        let token = codec.encode(&cursor).expect("encode");

        let decoded: TestCursor = codec.decode(&token).expect("decode");
        assert_eq!(decoded, cursor);

        // Tamper with signature
        let (payload_part, sig_part) = token.split_once('.').expect("dot separator");
        let mut tampered_sig = sig_part.as_bytes().to_vec();
        tampered_sig[0] = if tampered_sig[0] == b'A' { b'B' } else { b'A' };
        let tampered_sig_token = format!(
            "{payload_part}.{}",
            String::from_utf8(tampered_sig).unwrap()
        );
        assert!(codec.decode::<TestCursor>(&tampered_sig_token).is_err());

        // Tamper with payload
        let mut tampered_payload = payload_part.as_bytes().to_vec();
        tampered_payload[0] = if tampered_payload[0] == b'e' {
            b'f'
        } else {
            b'e'
        };
        let tampered_payload_token = format!(
            "{}.{sig_part}",
            String::from_utf8(tampered_payload).unwrap()
        );
        assert!(codec.decode::<TestCursor>(&tampered_payload_token).is_err());

        // Wrong key
        let wrong_codec = CursorCodec::hmac_sha256(b"different-secret-signing-key-xyz".to_vec());
        assert!(wrong_codec.decode::<TestCursor>(&token).is_err());
    }

    #[test]
    fn opaque_cursor_codec_roundtrips() {
        let codec = CursorCodec::opaque();
        let cursor = TestCursor { id: 99 };
        let token = codec.encode(&cursor).expect("encode");
        let decoded: TestCursor = codec.decode(&token).expect("decode");
        assert_eq!(decoded, cursor);
    }

    #[derive(Debug, PartialEq, Eq)]
    struct ItemRow {
        id: i64,
        name: String,
    }

    impl FromRow for ItemRow {
        fn from_row(row: &DbRow) -> Result<Self, PersistenceError> {
            Ok(Self {
                id: row.get("id")?,
                name: row.get("name")?,
            })
        }
    }

    #[tokio::test]
    async fn dynamic_select_paginates_offset_and_cursor_against_sqlite() {
        let backend = SqliteBackend::in_memory().await.expect("sqlite in-memory");
        let pool = DatabasePool::new(backend, DatabaseConfig::new("sqlite::memory:"));
        pool.execute("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
            .await
            .expect("create table");
        for i in 1..=5_i64 {
            pool.execute_params(
                "INSERT INTO items (id, name) VALUES (?, ?)",
                &[i.into(), format!("item-{i}").into()],
            )
            .await
            .expect("insert");
        }

        let mut base = Query::select();
        base.columns([Alias::new("id"), Alias::new("name")])
            .from(Alias::new("items"))
            .order_by(Alias::new("id"), Order::Asc);
        let select = DynamicSelect::new(base);

        let page2: OffsetPage<ItemRow> = select
            .paginate_offset(&pool, &OffsetPageRequest::new(2, 2))
            .await
            .expect("paginate_offset");
        assert_eq!(page2.total, 5);
        assert_eq!(page2.total_pages, 3);
        assert!(page2.has_next);
        assert!(page2.has_prev);
        assert_eq!(page2.items.len(), 2);
        assert_eq!(page2.items[0].id, 3);
        assert_eq!(page2.items[1].id, 4);

        let codec = CursorCodec::hmac_sha256(b"test-key".to_vec());
        let first_cursor_page: CursorPage<ItemRow> = select
            .paginate_cursor(
                &pool,
                &CursorPageRequest::new(None, 3),
                &codec,
                |r: &ItemRow| TestCursor { id: r.id },
            )
            .await
            .expect("paginate_cursor");
        assert!(first_cursor_page.has_next);
        assert_eq!(first_cursor_page.items.len(), 3);
        assert!(first_cursor_page.prev_cursor.is_none());
        let next_tok = first_cursor_page.next_cursor.expect("next_cursor");
        let decoded: TestCursor = codec.decode(&next_tok).expect("decode next_cursor");
        assert_eq!(decoded.id, 3);
    }

    #[test]
    fn cursor_codec_debug_redacts_hmac_key() {
        let codec = CursorCodec::hmac_sha256(b"top-secret-cursor-key".to_vec());
        let dbg = format!("{codec:?}");
        assert!(dbg.contains("[REDACTED]"));
        assert!(!dbg.contains("top-secret"));
        assert!(!dbg.contains("116, 111, 112"));
        assert_eq!(format!("{:?}", CursorCodec::opaque()), "Opaque");
    }
}
