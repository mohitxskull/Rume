//! Ticket and session authentication/tenancy extractors for real-time endpoints.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use rume_security::auth::{AuthUser, Session, SignedTokenClaims, TokenSigner};
use rume_security::tenant::Tenant;

use crate::realtime::hub::RealtimeError;

const REALTIME_TICKET_PURPOSE: &str = "realtime";

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Verified claims extracted from a real-time ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealtimeTicket {
    /// Authenticated user or subject ID.
    pub subject: String,
    /// Verified tenant ID bound to the ticket.
    pub tenant_id: String,
    /// Optional room restriction.
    pub room: Option<String>,
    /// Expiration timestamp (UNIX epoch seconds).
    pub expires_at: u64,
}

/// HMAC-SHA256 signer and verifier for [`RealtimeTicket`]s.
#[derive(Debug, Clone)]
pub struct RealtimeTicketSigner {
    signer: TokenSigner,
}

impl RealtimeTicketSigner {
    /// Creates a new ticket signer (minimum 32-byte secret).
    pub fn new(secret: impl AsRef<[u8]>) -> Result<Self, RealtimeError> {
        let signer =
            TokenSigner::new(secret).map_err(|e| RealtimeError::Unauthorized(e.to_string()))?;
        Ok(Self { signer })
    }

    /// Issues a signed ticket string for `(subject, tenant_id)` with optional room scope.
    pub fn issue(
        &self,
        subject: impl Into<String>,
        tenant_id: impl Into<String>,
        room: Option<String>,
        ttl_secs: u64,
    ) -> Result<String, RealtimeError> {
        let tenant_id = tenant_id.into();
        if tenant_id.is_empty() || tenant_id.contains(':') {
            return Err(RealtimeError::InvalidIdentifier {
                kind: "tenant_id",
                value: tenant_id,
            });
        }
        let now = now_epoch_secs();
        let mut metadata = BTreeMap::new();
        metadata.insert("tenant_id".to_string(), tenant_id);
        if let Some(r) = room {
            if r.is_empty() || r.contains(':') {
                return Err(RealtimeError::InvalidIdentifier {
                    kind: "room",
                    value: r,
                });
            }
            metadata.insert("room".to_string(), r);
        }

        let claims = SignedTokenClaims {
            purpose: REALTIME_TICKET_PURPOSE.to_string(),
            subject: subject.into(),
            expires_at: now.saturating_add(ttl_secs.max(1)),
            issued_at: now,
            nonce: uuid::Uuid::new_v4().to_string(),
            metadata,
        };
        self.signer
            .sign(&claims)
            .map_err(|e| RealtimeError::Unauthorized(e.to_string()))
    }

    /// Verifies a signed ticket string.
    pub fn verify(&self, token: &str) -> Result<RealtimeTicket, RealtimeError> {
        let claims = self
            .signer
            .verify(token, REALTIME_TICKET_PURPOSE, now_epoch_secs())
            .map_err(|e| RealtimeError::Unauthorized(e.to_string()))?;
        let tenant_id = claims
            .metadata
            .get("tenant_id")
            .filter(|s| !s.is_empty())
            .cloned()
            .ok_or_else(|| RealtimeError::Unauthorized("ticket missing tenant_id".to_string()))?;
        let room = claims.metadata.get("room").cloned();
        Ok(RealtimeTicket {
            subject: claims.subject,
            tenant_id,
            room,
            expires_at: claims.expires_at,
        })
    }
}

/// Axum extractor authenticating a real-time WebSocket or SSE request and binding its `tenant_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealtimeAuth {
    /// Authenticated subject ID.
    pub subject: String,
    /// Verified tenant ID (never trusted from client-supplied query/header params).
    pub tenant_id: String,
    /// Optional room restriction from a scoped ticket.
    pub room: Option<String>,
}

impl RealtimeAuth {
    /// Verifies that the authenticated identity is permitted to join `room`.
    pub fn authorize_room(&self, room: &str) -> Result<(), RealtimeError> {
        if let Some(ref allowed) = self.room
            && allowed != room
        {
            return Err(RealtimeError::Forbidden(format!(
                "ticket scoped to room {allowed:?}, cannot access {room:?}"
            )));
        }
        Ok(())
    }
}

fn extract_query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key && !v.is_empty()).then_some(v)
    })
}

fn check_cross_tenant_override(
    parts: &Parts,
    verified_tenant_id: &str,
) -> Result<(), RealtimeError> {
    if let Some(tenant) = parts.extensions.get::<Tenant>()
        && tenant.id != verified_tenant_id
    {
        return Err(RealtimeError::Forbidden(
            "tenant context does not match authenticated tenant".to_string(),
        ));
    }
    if let Some(query) = parts.uri.query()
        && let Some(client_tenant) = extract_query_param(query, "tenant_id")
        && client_tenant != verified_tenant_id
    {
        return Err(RealtimeError::Forbidden(
            "client-supplied tenant_id does not match authenticated tenant".to_string(),
        ));
    }
    Ok(())
}

impl<S: Send + Sync> FromRequestParts<S> for RealtimeAuth {
    type Rejection = RealtimeError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let bearer_token = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| {
                s.strip_prefix("Bearer ")
                    .or_else(|| s.strip_prefix("bearer "))
            })
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let query_token = parts
            .uri
            .query()
            .and_then(|q| extract_query_param(q, "ticket"));

        if let Some(token) = bearer_token.or(query_token) {
            let signer = parts
                .extensions
                .get::<RealtimeTicketSigner>()
                .ok_or_else(|| {
                    RealtimeError::Unauthorized("realtime ticket signer not configured".to_string())
                })?;
            let ticket = signer.verify(token)?;
            check_cross_tenant_override(parts, &ticket.tenant_id)?;
            return Ok(Self {
                subject: ticket.subject,
                tenant_id: ticket.tenant_id,
                room: ticket.room,
            });
        }

        if let Some(user) = parts.extensions.get::<AuthUser>() {
            let tenant_id = user.tenant_id.clone().ok_or_else(|| {
                RealtimeError::Forbidden("authenticated user has no tenant_id".to_string())
            })?;
            check_cross_tenant_override(parts, &tenant_id)?;
            return Ok(Self {
                subject: user.id.clone(),
                tenant_id,
                room: None,
            });
        }

        if let Some(session) = parts.extensions.get::<Session>() {
            if session.is_expired() {
                return Err(RealtimeError::Unauthorized("session expired".to_string()));
            }
            let tenant_id = session.tenant_id.clone().ok_or_else(|| {
                RealtimeError::Forbidden("authenticated session has no tenant_id".to_string())
            })?;
            check_cross_tenant_override(parts, &tenant_id)?;
            return Ok(Self {
                subject: session.user_id.clone(),
                tenant_id,
                room: None,
            });
        }

        tracing::debug!(
            uri = %redact_ticket_query(&parts.uri.to_string()),
            "realtime authentication failed: missing ticket or session"
        );
        Err(RealtimeError::Unauthorized(
            "missing realtime ticket or authenticated session".to_string(),
        ))
    }
}

/// Redacts `?ticket=<value>` / `&ticket=<value>` query parameters from a URI string for safe logging.
pub fn redact_ticket_query(uri: &str) -> String {
    let Some((path, query)) = uri.split_once('?') else {
        return uri.to_string();
    };
    let redacted_pairs: Vec<String> = query
        .split('&')
        .map(|pair| {
            if let Some((k, _)) = pair.split_once('=')
                && k == "ticket"
            {
                "ticket=[REDACTED]".to_string()
            } else {
                pair.to_string()
            }
        })
        .collect();
    format!("{path}?{}", redacted_pairs.join("&"))
}
