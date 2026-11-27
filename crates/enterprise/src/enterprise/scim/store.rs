//! SCIM 2.0 resource storage trait and in-memory implementation.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    SCIM_GROUP_SCHEMA, SCIM_PATCH_SCHEMA, SCIM_USER_SCHEMA, ScimEmail, ScimErrorResponse,
    ScimFilter, ScimGroup, ScimListResponse, ScimMember, ScimMeta, ScimName, ScimRole, ScimUser,
};

/// Default page size when `count` is omitted from a SCIM list query.
const DEFAULT_LIST_COUNT: usize = 100;

/// Pagination and filtering query parameters for SCIM list endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScimListQuery {
    pub filter: Option<String>,
    pub start_index: usize,
    pub count: Option<usize>,
}

impl Default for ScimListQuery {
    fn default() -> Self {
        Self {
            filter: None,
            start_index: 1,
            count: None,
        }
    }
}

/// A single RFC 7644 §3.5.2 PATCH operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScimPatchOp {
    pub op: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub value: Option<serde_json::Value>,
}

/// RFC 7644 §3.5.2 PatchOp request body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScimPatchRequest {
    #[serde(default = "default_patch_schemas")]
    pub schemas: Vec<String>,
    #[serde(rename = "Operations")]
    pub operations: Vec<ScimPatchOp>,
}

fn default_patch_schemas() -> Vec<String> {
    vec![SCIM_PATCH_SCHEMA.to_string()]
}

/// Errors returned by [`ScimStore`] operations.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ScimStoreError {
    #[error("{resource_type} '{id}' not found")]
    NotFound {
        resource_type: &'static str,
        id: String,
    },
    #[error("conflict: {detail}")]
    Conflict { detail: String },
    #[error("precondition failed: {detail}")]
    PreconditionFailed { detail: String },
    #[error("bad request: {detail}")]
    BadRequest {
        scim_type: Option<&'static str>,
        detail: String,
    },
    #[error("internal store error: {0}")]
    Internal(String),
}

impl ScimStoreError {
    /// Returns the HTTP status code corresponding to this error.
    pub fn status_code(&self) -> u16 {
        match self {
            Self::NotFound { .. } => 404,
            Self::Conflict { .. } => 409,
            Self::PreconditionFailed { .. } => 412,
            Self::BadRequest { .. } => 400,
            Self::Internal(_) => 500,
        }
    }

    /// Converts this error into an RFC 7644 [`ScimErrorResponse`].
    pub fn to_scim_error(&self) -> ScimErrorResponse {
        match self {
            Self::NotFound { .. } => ScimErrorResponse::new(404, None, Some(&self.to_string())),
            Self::Conflict { detail } => {
                ScimErrorResponse::new(409, Some("uniqueness"), Some(detail))
            }
            Self::PreconditionFailed { detail } => ScimErrorResponse::new(412, None, Some(detail)),
            Self::BadRequest { scim_type, detail } => {
                ScimErrorResponse::new(400, *scim_type, Some(detail))
            }
            Self::Internal(detail) => ScimErrorResponse::new(500, None, Some(detail)),
        }
    }
}

/// Evaluates an RFC 9110 / RFC 7644 `If-Match` header value (possibly comma-folded from
/// multiple header lines) against a resource's current ETag using quote-aware splitting
/// so commas inside quoted entity tags (e.g. `"v1,2"`) are preserved intact (`P2-08`).
pub fn if_match_satisfies(if_match: &str, current_etag: Option<&str>) -> bool {
    let mut candidates: Vec<&str> = Vec::new();
    let mut start = 0usize;
    let mut in_quotes = false;
    let bytes = if_match.as_bytes();
    for (idx, &b) in bytes.iter().enumerate() {
        match b {
            b'"' => in_quotes = !in_quotes,
            b',' if !in_quotes => {
                let token = if_match[start..idx].trim();
                if !token.is_empty() {
                    candidates.push(token);
                }
                start = idx + 1;
            }
            _ => {}
        }
    }
    if in_quotes {
        return false;
    }
    let tail = if_match[start..].trim();
    if !tail.is_empty() {
        candidates.push(tail);
    }
    if candidates.is_empty() {
        return false;
    }

    let Some(current) = current_etag else {
        return candidates.contains(&"*");
    };
    let current_norm = current.trim().strip_prefix("W/").unwrap_or(current.trim());
    candidates.into_iter().any(|candidate| {
        if candidate == "*" {
            return true;
        }
        let cand_norm = candidate.strip_prefix("W/").unwrap_or(candidate);
        cand_norm == current_norm
    })
}

fn check_expected_version(
    expected_version: Option<&str>,
    current_version: Option<&str>,
) -> Result<(), ScimStoreError> {
    if let Some(expected) = expected_version
        && !if_match_satisfies(expected, current_version)
    {
        return Err(ScimStoreError::PreconditionFailed {
            detail: "resource version does not match If-Match precondition".into(),
        });
    }
    Ok(())
}

/// Asynchronous SCIM 2.0 User and Group persistence interface.
pub trait ScimStore: Send + Sync {
    fn create_user(&self, user: ScimUser) -> BoxFuture<'_, Result<ScimUser, ScimStoreError>>;
    fn get_user(&self, id: &str) -> BoxFuture<'_, Result<ScimUser, ScimStoreError>>;
    fn replace_user<'a>(
        &'a self,
        id: &'a str,
        user: ScimUser,
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<ScimUser, ScimStoreError>>;
    fn patch_user<'a>(
        &'a self,
        id: &'a str,
        patch: ScimPatchRequest,
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<ScimUser, ScimStoreError>>;
    fn delete_user<'a>(
        &'a self,
        id: &'a str,
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), ScimStoreError>>;
    fn list_users(
        &self,
        query: ScimListQuery,
    ) -> BoxFuture<'_, Result<ScimListResponse<ScimUser>, ScimStoreError>>;

    fn create_group(&self, group: ScimGroup) -> BoxFuture<'_, Result<ScimGroup, ScimStoreError>>;
    fn get_group(&self, id: &str) -> BoxFuture<'_, Result<ScimGroup, ScimStoreError>>;
    fn replace_group<'a>(
        &'a self,
        id: &'a str,
        group: ScimGroup,
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<ScimGroup, ScimStoreError>>;
    fn patch_group<'a>(
        &'a self,
        id: &'a str,
        patch: ScimPatchRequest,
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<ScimGroup, ScimStoreError>>;
    fn delete_group<'a>(
        &'a self,
        id: &'a str,
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), ScimStoreError>>;
    fn list_groups(
        &self,
        query: ScimListQuery,
    ) -> BoxFuture<'_, Result<ScimListResponse<ScimGroup>, ScimStoreError>>;
}

impl<T: ScimStore + ?Sized> ScimStore for Arc<T> {
    fn create_user(&self, user: ScimUser) -> BoxFuture<'_, Result<ScimUser, ScimStoreError>> {
        (**self).create_user(user)
    }
    fn get_user(&self, id: &str) -> BoxFuture<'_, Result<ScimUser, ScimStoreError>> {
        (**self).get_user(id)
    }
    fn replace_user<'a>(
        &'a self,
        id: &'a str,
        user: ScimUser,
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<ScimUser, ScimStoreError>> {
        (**self).replace_user(id, user, expected_version)
    }
    fn patch_user<'a>(
        &'a self,
        id: &'a str,
        patch: ScimPatchRequest,
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<ScimUser, ScimStoreError>> {
        (**self).patch_user(id, patch, expected_version)
    }
    fn delete_user<'a>(
        &'a self,
        id: &'a str,
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), ScimStoreError>> {
        (**self).delete_user(id, expected_version)
    }
    fn list_users(
        &self,
        query: ScimListQuery,
    ) -> BoxFuture<'_, Result<ScimListResponse<ScimUser>, ScimStoreError>> {
        (**self).list_users(query)
    }
    fn create_group(&self, group: ScimGroup) -> BoxFuture<'_, Result<ScimGroup, ScimStoreError>> {
        (**self).create_group(group)
    }
    fn get_group(&self, id: &str) -> BoxFuture<'_, Result<ScimGroup, ScimStoreError>> {
        (**self).get_group(id)
    }
    fn replace_group<'a>(
        &'a self,
        id: &'a str,
        group: ScimGroup,
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<ScimGroup, ScimStoreError>> {
        (**self).replace_group(id, group, expected_version)
    }
    fn patch_group<'a>(
        &'a self,
        id: &'a str,
        patch: ScimPatchRequest,
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<ScimGroup, ScimStoreError>> {
        (**self).patch_group(id, patch, expected_version)
    }
    fn delete_group<'a>(
        &'a self,
        id: &'a str,
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), ScimStoreError>> {
        (**self).delete_group(id, expected_version)
    }
    fn list_groups(
        &self,
        query: ScimListQuery,
    ) -> BoxFuture<'_, Result<ScimListResponse<ScimGroup>, ScimStoreError>> {
        (**self).list_groups(query)
    }
}

#[derive(Debug, Default)]
struct StoreInner {
    users: HashMap<String, ScimUser>,
    groups: HashMap<String, ScimGroup>,
    versions: HashMap<String, u64>,
}

impl StoreInner {
    fn next_version(&mut self, key: &str) -> String {
        let entry = self.versions.entry(key.to_string()).or_insert(0);
        *entry += 1;
        format!("W/\"v{}\"", *entry)
    }
}

/// Thread-safe in-memory [`ScimStore`] for testing and embedded provisioning.
#[derive(Debug, Clone, Default)]
pub struct InMemoryScimStore {
    inner: Arc<RwLock<StoreInner>>,
}

impl InMemoryScimStore {
    /// Creates an empty in-memory SCIM store.
    pub fn new() -> Self {
        Self::default()
    }
}

fn parse_optional_filter(filter: Option<&str>) -> Result<Option<ScimFilter>, ScimStoreError> {
    filter
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            ScimFilter::parse(s).map_err(|e| ScimStoreError::BadRequest {
                scim_type: Some("invalidFilter"),
                detail: e.to_string(),
            })
        })
        .transpose()
}

fn paginate<T>(mut items: Vec<T>, query: &ScimListQuery) -> ScimListResponse<T> {
    let total_results = items.len();
    let start_index = query.start_index.max(1);
    let offset = start_index - 1;
    let count = query.count.unwrap_or(DEFAULT_LIST_COUNT);

    let page = if offset >= total_results || count == 0 {
        Vec::new()
    } else {
        items.drain(offset..).take(count).collect()
    };

    ScimListResponse::new(page, total_results, start_index)
}

fn stamp_meta(
    meta: Option<ScimMeta>,
    resource_type: &str,
    location: String,
    version: String,
) -> Option<ScimMeta> {
    let mut m = meta.unwrap_or_else(|| ScimMeta {
        resource_type: resource_type.to_string(),
        ..Default::default()
    });
    if m.resource_type.is_empty() {
        m.resource_type = resource_type.to_string();
    }
    m.location = Some(location);
    m.version = Some(version);
    Some(m)
}

fn apply_user_patch_op(user: &mut ScimUser, op: &ScimPatchOp) -> Result<(), ScimStoreError> {
    let verb = op.op.to_ascii_lowercase();
    if !matches!(verb.as_str(), "add" | "replace" | "remove") {
        return Err(ScimStoreError::BadRequest {
            scim_type: Some("invalidSyntax"),
            detail: format!("unsupported patch op '{}'", op.op),
        });
    }

    match op.path.as_deref().map(str::to_ascii_lowercase).as_deref() {
        None => {
            if verb == "remove" {
                return Err(ScimStoreError::BadRequest {
                    scim_type: Some("noTarget"),
                    detail: "remove operation requires a path".into(),
                });
            }
            let obj = op
                .value
                .as_ref()
                .and_then(|v| v.as_object())
                .ok_or_else(|| ScimStoreError::BadRequest {
                    scim_type: Some("invalidValue"),
                    detail: "patch without path requires an object value".into(),
                })?;
            for (k, v) in obj {
                if let Some(sub_obj) = v.as_object() {
                    for (sub_k, sub_v) in sub_obj {
                        apply_user_patch_op(
                            user,
                            &ScimPatchOp {
                                op: op.op.clone(),
                                path: Some(format!("{k}.{sub_k}")),
                                value: Some(sub_v.clone()),
                            },
                        )?;
                    }
                } else {
                    apply_user_patch_op(
                        user,
                        &ScimPatchOp {
                            op: op.op.clone(),
                            path: Some(k.clone()),
                            value: Some(v.clone()),
                        },
                    )?;
                }
            }
            Ok(())
        }
        Some("active") => {
            if verb == "remove" {
                user.active = false;
            } else {
                user.active = op.value.as_ref().and_then(|v| v.as_bool()).ok_or_else(|| {
                    ScimStoreError::BadRequest {
                        scim_type: Some("invalidValue"),
                        detail: "active must be a boolean".into(),
                    }
                })?;
            }
            Ok(())
        }
        Some("username") => {
            if verb == "remove" {
                return Err(ScimStoreError::BadRequest {
                    scim_type: Some("mutability"),
                    detail: "userName cannot be removed".into(),
                });
            }
            let new_name = op
                .value
                .as_ref()
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .ok_or_else(|| ScimStoreError::BadRequest {
                    scim_type: Some("invalidValue"),
                    detail: "userName must be a non-empty string".into(),
                })?;
            user.user_name = new_name;
            Ok(())
        }
        Some("displayname") => {
            user.display_name = if verb == "remove" {
                None
            } else {
                Some(
                    op.value
                        .as_ref()
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .ok_or_else(|| ScimStoreError::BadRequest {
                            scim_type: Some("invalidValue"),
                            detail: "displayName must be a string".into(),
                        })?,
                )
            };
            Ok(())
        }
        Some("externalid") => {
            user.external_id = if verb == "remove" {
                None
            } else {
                Some(
                    op.value
                        .as_ref()
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .ok_or_else(|| ScimStoreError::BadRequest {
                            scim_type: Some("invalidValue"),
                            detail: "externalId must be a string".into(),
                        })?,
                )
            };
            Ok(())
        }
        Some("name.formatted") | Some("name.familyname") | Some("name.givenname") => {
            let path = op.path.as_deref().unwrap_or("").to_ascii_lowercase();
            let name = user.name.get_or_insert_with(ScimName::default);
            let val = if verb == "remove" {
                None
            } else {
                Some(
                    op.value
                        .as_ref()
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .ok_or_else(|| ScimStoreError::BadRequest {
                            scim_type: Some("invalidValue"),
                            detail: "name sub-attribute must be a string".into(),
                        })?,
                )
            };
            match path.as_str() {
                "name.formatted" => name.formatted = val,
                "name.familyname" => name.family_name = val,
                "name.givenname" => name.given_name = val,
                _ => {}
            }
            Ok(())
        }
        Some("emails") => {
            if verb == "remove" {
                user.emails.clear();
            } else {
                let emails: Vec<ScimEmail> = op
                    .value
                    .as_ref()
                    .ok_or_else(|| ScimStoreError::BadRequest {
                        scim_type: Some("invalidValue"),
                        detail: "emails patch requires a value".into(),
                    })
                    .and_then(|v| {
                        serde_json::from_value(v.clone()).map_err(|e| ScimStoreError::BadRequest {
                            scim_type: Some("invalidValue"),
                            detail: e.to_string(),
                        })
                    })?;
                if verb == "add" {
                    user.emails.extend(emails);
                } else {
                    user.emails = emails;
                }
            }
            Ok(())
        }
        Some("roles") => {
            if verb == "remove" {
                user.roles.clear();
            } else {
                let roles: Vec<ScimRole> = op
                    .value
                    .as_ref()
                    .ok_or_else(|| ScimStoreError::BadRequest {
                        scim_type: Some("invalidValue"),
                        detail: "roles patch requires a value".into(),
                    })
                    .and_then(|v| {
                        serde_json::from_value(v.clone()).map_err(|e| ScimStoreError::BadRequest {
                            scim_type: Some("invalidValue"),
                            detail: e.to_string(),
                        })
                    })?;
                if verb == "add" {
                    user.roles.extend(roles);
                } else {
                    user.roles = roles;
                }
            }
            Ok(())
        }
        Some(other) => Err(ScimStoreError::BadRequest {
            scim_type: Some("invalidPath"),
            detail: format!("unsupported User patch path '{other}'"),
        }),
    }
}

fn apply_group_patch_op(group: &mut ScimGroup, op: &ScimPatchOp) -> Result<(), ScimStoreError> {
    let verb = op.op.to_ascii_lowercase();
    if !matches!(verb.as_str(), "add" | "replace" | "remove") {
        return Err(ScimStoreError::BadRequest {
            scim_type: Some("invalidSyntax"),
            detail: format!("unsupported patch op '{}'", op.op),
        });
    }

    match op.path.as_deref().map(str::to_ascii_lowercase).as_deref() {
        None => {
            if verb == "remove" {
                return Err(ScimStoreError::BadRequest {
                    scim_type: Some("noTarget"),
                    detail: "remove operation requires a path".into(),
                });
            }
            let obj = op
                .value
                .as_ref()
                .and_then(|v| v.as_object())
                .ok_or_else(|| ScimStoreError::BadRequest {
                    scim_type: Some("invalidValue"),
                    detail: "patch without path requires an object value".into(),
                })?;
            for (k, v) in obj {
                apply_group_patch_op(
                    group,
                    &ScimPatchOp {
                        op: op.op.clone(),
                        path: Some(k.clone()),
                        value: Some(v.clone()),
                    },
                )?;
            }
            Ok(())
        }
        Some("displayname") => {
            if verb == "remove" {
                return Err(ScimStoreError::BadRequest {
                    scim_type: Some("mutability"),
                    detail: "displayName cannot be removed".into(),
                });
            }
            let new_name = op
                .value
                .as_ref()
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .ok_or_else(|| ScimStoreError::BadRequest {
                    scim_type: Some("invalidValue"),
                    detail: "displayName must be a non-empty string".into(),
                })?;
            group.display_name = new_name;
            Ok(())
        }
        Some("members") => {
            if verb == "remove" {
                group.members.clear();
            } else {
                let members: Vec<ScimMember> = op
                    .value
                    .as_ref()
                    .ok_or_else(|| ScimStoreError::BadRequest {
                        scim_type: Some("invalidValue"),
                        detail: "members patch requires a value".into(),
                    })
                    .and_then(|v| {
                        serde_json::from_value(v.clone()).map_err(|e| ScimStoreError::BadRequest {
                            scim_type: Some("invalidValue"),
                            detail: e.to_string(),
                        })
                    })?;
                if verb == "add" {
                    for member in members {
                        if !group
                            .members
                            .iter()
                            .any(|existing| existing.value == member.value)
                        {
                            group.members.push(member);
                        }
                    }
                } else {
                    group.members = members;
                }
            }
            Ok(())
        }
        Some(other) => Err(ScimStoreError::BadRequest {
            scim_type: Some("invalidPath"),
            detail: format!("unsupported Group patch path '{other}'"),
        }),
    }
}

impl ScimStore for InMemoryScimStore {
    fn create_user(&self, mut user: ScimUser) -> BoxFuture<'_, Result<ScimUser, ScimStoreError>> {
        Box::pin(async move {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| ScimStoreError::Internal(e.to_string()))?;

            if user.user_name.trim().is_empty() {
                return Err(ScimStoreError::BadRequest {
                    scim_type: Some("invalidValue"),
                    detail: "userName cannot be empty".into(),
                });
            }
            if user.id.is_empty() {
                user.id = uuid::Uuid::new_v4().to_string();
            }
            if inner.users.contains_key(&user.id) {
                return Err(ScimStoreError::Conflict {
                    detail: format!("User with id '{}' already exists", user.id),
                });
            }
            if inner
                .users
                .values()
                .any(|u| u.user_name.eq_ignore_ascii_case(&user.user_name))
            {
                return Err(ScimStoreError::Conflict {
                    detail: format!("User with userName '{}' already exists", user.user_name),
                });
            }
            if user.schemas.is_empty() {
                user.schemas.push(SCIM_USER_SCHEMA.to_string());
            }
            let ver = inner.next_version(&format!("user:{}", user.id));
            user.meta = stamp_meta(
                user.meta.take(),
                "User",
                format!("/scim/v2/Users/{}", user.id),
                ver,
            );
            inner.users.insert(user.id.clone(), user.clone());
            Ok(user)
        })
    }

    fn get_user(&self, id: &str) -> BoxFuture<'_, Result<ScimUser, ScimStoreError>> {
        let id = id.to_string();
        Box::pin(async move {
            let inner = self
                .inner
                .read()
                .map_err(|e| ScimStoreError::Internal(e.to_string()))?;
            inner
                .users
                .get(&id)
                .cloned()
                .ok_or(ScimStoreError::NotFound {
                    resource_type: "User",
                    id,
                })
        })
    }

    fn replace_user<'a>(
        &'a self,
        id: &'a str,
        mut user: ScimUser,
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<ScimUser, ScimStoreError>> {
        let id = id.to_string();
        Box::pin(async move {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| ScimStoreError::Internal(e.to_string()))?;
            if user.user_name.trim().is_empty() {
                return Err(ScimStoreError::BadRequest {
                    scim_type: Some("invalidValue"),
                    detail: "userName cannot be empty".into(),
                });
            }
            let existing = inner
                .users
                .get(&id)
                .ok_or_else(|| ScimStoreError::NotFound {
                    resource_type: "User",
                    id: id.clone(),
                })?;
            check_expected_version(
                expected_version,
                existing.meta.as_ref().and_then(|m| m.version.as_deref()),
            )?;
            if inner
                .users
                .values()
                .any(|u| u.id != id && u.user_name.eq_ignore_ascii_case(&user.user_name))
            {
                return Err(ScimStoreError::Conflict {
                    detail: format!("User with userName '{}' already exists", user.user_name),
                });
            }
            user.id = id.clone();
            if user.schemas.is_empty() {
                user.schemas.push(SCIM_USER_SCHEMA.to_string());
            }
            let ver = inner.next_version(&format!("user:{id}"));
            user.meta = stamp_meta(
                user.meta.take(),
                "User",
                format!("/scim/v2/Users/{id}"),
                ver,
            );
            inner.users.insert(id, user.clone());
            Ok(user)
        })
    }

    fn patch_user<'a>(
        &'a self,
        id: &'a str,
        patch: ScimPatchRequest,
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<ScimUser, ScimStoreError>> {
        let id = id.to_string();
        Box::pin(async move {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| ScimStoreError::Internal(e.to_string()))?;
            let mut user =
                inner
                    .users
                    .get(&id)
                    .cloned()
                    .ok_or_else(|| ScimStoreError::NotFound {
                        resource_type: "User",
                        id: id.clone(),
                    })?;
            check_expected_version(
                expected_version,
                user.meta.as_ref().and_then(|m| m.version.as_deref()),
            )?;
            for op in &patch.operations {
                apply_user_patch_op(&mut user, op)?;
            }
            if inner
                .users
                .values()
                .any(|u| u.id != id && u.user_name.eq_ignore_ascii_case(&user.user_name))
            {
                return Err(ScimStoreError::Conflict {
                    detail: format!("User with userName '{}' already exists", user.user_name),
                });
            }
            let ver = inner.next_version(&format!("user:{id}"));
            user.meta = stamp_meta(
                user.meta.take(),
                "User",
                format!("/scim/v2/Users/{id}"),
                ver,
            );
            inner.users.insert(id, user.clone());
            Ok(user)
        })
    }

    fn delete_user<'a>(
        &'a self,
        id: &'a str,
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), ScimStoreError>> {
        let id = id.to_string();
        Box::pin(async move {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| ScimStoreError::Internal(e.to_string()))?;
            let existing = inner
                .users
                .get(&id)
                .ok_or_else(|| ScimStoreError::NotFound {
                    resource_type: "User",
                    id: id.clone(),
                })?;
            check_expected_version(
                expected_version,
                existing.meta.as_ref().and_then(|m| m.version.as_deref()),
            )?;
            inner.users.remove(&id);
            inner.versions.remove(&format!("user:{id}"));
            Ok(())
        })
    }

    fn list_users(
        &self,
        query: ScimListQuery,
    ) -> BoxFuture<'_, Result<ScimListResponse<ScimUser>, ScimStoreError>> {
        Box::pin(async move {
            let parsed = parse_optional_filter(query.filter.as_deref())?;
            let inner = self
                .inner
                .read()
                .map_err(|e| ScimStoreError::Internal(e.to_string()))?;
            let mut matched: Vec<ScimUser> = inner
                .users
                .values()
                .filter(|u| parsed.as_ref().is_none_or(|f| f.matches_user(u)))
                .cloned()
                .collect();
            matched.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(paginate(matched, &query))
        })
    }

    fn create_group(
        &self,
        mut group: ScimGroup,
    ) -> BoxFuture<'_, Result<ScimGroup, ScimStoreError>> {
        Box::pin(async move {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| ScimStoreError::Internal(e.to_string()))?;

            if group.display_name.trim().is_empty() {
                return Err(ScimStoreError::BadRequest {
                    scim_type: Some("invalidValue"),
                    detail: "displayName cannot be empty".into(),
                });
            }
            if group.id.is_empty() {
                group.id = uuid::Uuid::new_v4().to_string();
            }
            if inner.groups.contains_key(&group.id) {
                return Err(ScimStoreError::Conflict {
                    detail: format!("Group with id '{}' already exists", group.id),
                });
            }
            if inner
                .groups
                .values()
                .any(|g| g.display_name.eq_ignore_ascii_case(&group.display_name))
            {
                return Err(ScimStoreError::Conflict {
                    detail: format!(
                        "Group with displayName '{}' already exists",
                        group.display_name
                    ),
                });
            }
            if group.schemas.is_empty() {
                group.schemas.push(SCIM_GROUP_SCHEMA.to_string());
            }
            let ver = inner.next_version(&format!("group:{}", group.id));
            group.meta = stamp_meta(
                group.meta.take(),
                "Group",
                format!("/scim/v2/Groups/{}", group.id),
                ver,
            );
            inner.groups.insert(group.id.clone(), group.clone());
            Ok(group)
        })
    }

    fn get_group(&self, id: &str) -> BoxFuture<'_, Result<ScimGroup, ScimStoreError>> {
        let id = id.to_string();
        Box::pin(async move {
            let inner = self
                .inner
                .read()
                .map_err(|e| ScimStoreError::Internal(e.to_string()))?;
            inner
                .groups
                .get(&id)
                .cloned()
                .ok_or(ScimStoreError::NotFound {
                    resource_type: "Group",
                    id,
                })
        })
    }

    fn replace_group<'a>(
        &'a self,
        id: &'a str,
        mut group: ScimGroup,
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<ScimGroup, ScimStoreError>> {
        let id = id.to_string();
        Box::pin(async move {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| ScimStoreError::Internal(e.to_string()))?;
            if group.display_name.trim().is_empty() {
                return Err(ScimStoreError::BadRequest {
                    scim_type: Some("invalidValue"),
                    detail: "displayName cannot be empty".into(),
                });
            }
            let existing = inner
                .groups
                .get(&id)
                .ok_or_else(|| ScimStoreError::NotFound {
                    resource_type: "Group",
                    id: id.clone(),
                })?;
            check_expected_version(
                expected_version,
                existing.meta.as_ref().and_then(|m| m.version.as_deref()),
            )?;
            if inner
                .groups
                .values()
                .any(|g| g.id != id && g.display_name.eq_ignore_ascii_case(&group.display_name))
            {
                return Err(ScimStoreError::Conflict {
                    detail: format!(
                        "Group with displayName '{}' already exists",
                        group.display_name
                    ),
                });
            }
            group.id = id.clone();
            if group.schemas.is_empty() {
                group.schemas.push(SCIM_GROUP_SCHEMA.to_string());
            }
            let ver = inner.next_version(&format!("group:{id}"));
            group.meta = stamp_meta(
                group.meta.take(),
                "Group",
                format!("/scim/v2/Groups/{id}"),
                ver,
            );
            inner.groups.insert(id, group.clone());
            Ok(group)
        })
    }

    fn patch_group<'a>(
        &'a self,
        id: &'a str,
        patch: ScimPatchRequest,
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<ScimGroup, ScimStoreError>> {
        let id = id.to_string();
        Box::pin(async move {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| ScimStoreError::Internal(e.to_string()))?;
            let mut group =
                inner
                    .groups
                    .get(&id)
                    .cloned()
                    .ok_or_else(|| ScimStoreError::NotFound {
                        resource_type: "Group",
                        id: id.clone(),
                    })?;
            check_expected_version(
                expected_version,
                group.meta.as_ref().and_then(|m| m.version.as_deref()),
            )?;
            for op in &patch.operations {
                apply_group_patch_op(&mut group, op)?;
            }
            if inner
                .groups
                .values()
                .any(|g| g.id != id && g.display_name.eq_ignore_ascii_case(&group.display_name))
            {
                return Err(ScimStoreError::Conflict {
                    detail: format!(
                        "Group with displayName '{}' already exists",
                        group.display_name
                    ),
                });
            }
            let ver = inner.next_version(&format!("group:{id}"));
            group.meta = stamp_meta(
                group.meta.take(),
                "Group",
                format!("/scim/v2/Groups/{id}"),
                ver,
            );
            inner.groups.insert(id, group.clone());
            Ok(group)
        })
    }

    fn delete_group<'a>(
        &'a self,
        id: &'a str,
        expected_version: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), ScimStoreError>> {
        let id = id.to_string();
        Box::pin(async move {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| ScimStoreError::Internal(e.to_string()))?;
            let existing = inner
                .groups
                .get(&id)
                .ok_or_else(|| ScimStoreError::NotFound {
                    resource_type: "Group",
                    id: id.clone(),
                })?;
            check_expected_version(
                expected_version,
                existing.meta.as_ref().and_then(|m| m.version.as_deref()),
            )?;
            inner.groups.remove(&id);
            inner.versions.remove(&format!("group:{id}"));
            Ok(())
        })
    }

    fn list_groups(
        &self,
        query: ScimListQuery,
    ) -> BoxFuture<'_, Result<ScimListResponse<ScimGroup>, ScimStoreError>> {
        Box::pin(async move {
            let parsed = parse_optional_filter(query.filter.as_deref())?;
            let inner = self
                .inner
                .read()
                .map_err(|e| ScimStoreError::Internal(e.to_string()))?;
            let mut matched: Vec<ScimGroup> = inner
                .groups
                .values()
                .filter(|g| parsed.as_ref().is_none_or(|f| f.matches_group(g)))
                .cloned()
                .collect();
            matched.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(paginate(matched, &query))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scim_store_cleans_versions_validates_names_patches_nested_and_dedups_members() {
        let store = InMemoryScimStore::new();

        // Empty userName / displayName rejected on create
        assert!(matches!(
            store.create_user(ScimUser::new("", "   ")).await,
            Err(ScimStoreError::BadRequest { .. })
        ));
        assert!(matches!(
            store.create_group(ScimGroup::new("", "")).await,
            Err(ScimStoreError::BadRequest { .. })
        ));

        let user = store.create_user(ScimUser::new("", "alice")).await.unwrap();
        // Empty userName rejected on replace
        assert!(matches!(
            store
                .replace_user(&user.id, ScimUser::new(&user.id, ""), None)
                .await,
            Err(ScimStoreError::BadRequest { .. })
        ));

        // Nested object traversal in pathless PATCH
        let patched = store
            .patch_user(
                &user.id,
                ScimPatchRequest {
                    schemas: vec![SCIM_PATCH_SCHEMA.to_string()],
                    operations: vec![ScimPatchOp {
                        op: "replace".to_string(),
                        path: None,
                        value: Some(serde_json::json!({
                            "name": { "givenName": "Alice", "familyName": "Smith" }
                        })),
                    }],
                },
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            patched.name.as_ref().and_then(|n| n.given_name.as_deref()),
            Some("Alice")
        );
        assert_eq!(
            patched.name.as_ref().and_then(|n| n.family_name.as_deref()),
            Some("Smith")
        );

        // Stale expected_version fails atomically with PreconditionFailed (412)
        assert!(matches!(
            store.delete_user(&user.id, Some("W/\"v1\"")).await,
            Err(ScimStoreError::PreconditionFailed { .. })
        ));

        // Delete removes version entry (no memory leak)
        let current_ver = patched
            .meta
            .as_ref()
            .and_then(|m| m.version.as_deref())
            .unwrap();
        store
            .delete_user(&user.id, Some(current_ver))
            .await
            .unwrap();
        assert!(
            !store
                .inner
                .read()
                .unwrap()
                .versions
                .contains_key(&format!("user:{}", user.id))
        );

        // Group member deduplication on `add` PATCH and version cleanup on delete
        let group = store.create_group(ScimGroup::new("", "Eng")).await.unwrap();
        assert!(matches!(
            store
                .replace_group(&group.id, ScimGroup::new(&group.id, "  "), None)
                .await,
            Err(ScimStoreError::BadRequest { .. })
        ));

        let add_member = || ScimPatchRequest {
            schemas: vec![SCIM_PATCH_SCHEMA.to_string()],
            operations: vec![ScimPatchOp {
                op: "add".to_string(),
                path: Some("members".to_string()),
                value: Some(serde_json::json!([{ "value": "u1", "display": "Alice" }])),
            }],
        };
        let g1 = store
            .patch_group(&group.id, add_member(), None)
            .await
            .unwrap();
        let g2 = store
            .patch_group(&group.id, add_member(), None)
            .await
            .unwrap();
        assert_eq!(g1.members.len(), 1);
        assert_eq!(g2.members.len(), 1);

        store.delete_group(&group.id, None).await.unwrap();
        assert!(
            !store
                .inner
                .read()
                .unwrap()
                .versions
                .contains_key(&format!("group:{}", group.id))
        );
    }

    #[test]
    fn if_match_satisfies_handles_quoted_commas_and_folded_lists() {
        // Comma inside quoted ETag must NOT split the entity tag.
        assert!(if_match_satisfies("\"v1,2\"", Some("\"v1,2\"")));
        assert!(!if_match_satisfies("\"v1,2\"", Some("\"v1\"")));
        assert!(!if_match_satisfies("\"v1,2\"", Some("\"2\"")));
        // Folded list with multiple ETags and weak prefix.
        assert!(if_match_satisfies(
            "\"other\", W/\"v1,2\", \"third\"",
            Some("\"v1,2\"")
        ));
        assert!(if_match_satisfies("*", Some("W/\"v1\"")));
        assert!(!if_match_satisfies("\"unclosed", Some("\"unclosed")));
    }
}
