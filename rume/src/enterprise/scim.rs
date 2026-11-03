//! SCIM 2.0 (RFC 7643 / RFC 7644) protocol models and response envelopes.

use serde::{Deserialize, Serialize};

/// Standard User schema URN.
pub const SCIM_USER_SCHEMA: &str = "urn:ietf:params:scim:schemas:core:2.0:User";
/// Standard Group schema URN.
pub const SCIM_GROUP_SCHEMA: &str = "urn:ietf:params:scim:schemas:core:2.0:Group";
/// Standard ListResponse message URN.
pub const SCIM_LIST_SCHEMA: &str = "urn:ietf:params:scim:api:messages:2.0:ListResponse";
/// Standard Error message URN.
pub const SCIM_ERROR_SCHEMA: &str = "urn:ietf:params:scim:api:messages:2.0:Error";

/// SCIM Name sub-attribute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ScimName {
    pub formatted: Option<String>,
    pub family_name: Option<String>,
    pub given_name: Option<String>,
}

/// SCIM Email multi-valued attribute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScimEmail {
    pub value: String,
    pub primary: bool,
    #[serde(rename = "type")]
    pub email_type: Option<String>,
}

/// SCIM Metadata sub-attribute (RFC 7643 Section 3.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ScimMeta {
    pub resource_type: String,
    pub created: Option<String>,
    pub last_modified: Option<String>,
    pub location: Option<String>,
    pub version: Option<String>,
}

impl ScimMeta {
    /// Configures the ETag version string (e.g. W/"v1.0").
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }
}

/// SCIM 2.0 User resource representation (RFC 7643).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScimUser {
    pub schemas: Vec<String>,
    pub id: String,
    pub external_id: Option<String>,
    pub user_name: String,
    pub name: Option<ScimName>,
    pub display_name: Option<String>,
    pub active: bool,
    pub emails: Vec<ScimEmail>,
    pub roles: Vec<String>,
    pub meta: Option<ScimMeta>,
}

impl ScimUser {
    /// Creates a new SCIM User with mandatory fields.
    pub fn new(id: impl Into<String>, user_name: impl Into<String>) -> Self {
        Self {
            schemas: vec![SCIM_USER_SCHEMA.to_string()],
            id: id.into(),
            external_id: None,
            user_name: user_name.into(),
            name: None,
            display_name: None,
            active: true,
            emails: Vec::new(),
            roles: Vec::new(),
            meta: Some(ScimMeta {
                resource_type: "User".to_string(),
                ..Default::default()
            }),
        }
    }

    /// Adds an email.
    pub fn with_email(mut self, email: impl Into<String>, primary: bool) -> Self {
        self.emails.push(ScimEmail {
            value: email.into(),
            primary,
            email_type: Some("work".to_string()),
        });
        self
    }
}

/// Group membership reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScimMember {
    pub value: String,
    #[serde(rename = "$ref")]
    pub reference: Option<String>,
    pub display: Option<String>,
}

/// SCIM 2.0 Group resource representation (RFC 7643).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScimGroup {
    pub schemas: Vec<String>,
    pub id: String,
    pub display_name: String,
    pub members: Vec<ScimMember>,
    pub meta: Option<ScimMeta>,
}

impl ScimGroup {
    /// Creates a new SCIM Group.
    pub fn new(id: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            schemas: vec![SCIM_GROUP_SCHEMA.to_string()],
            id: id.into(),
            display_name: display_name.into(),
            members: Vec::new(),
            meta: Some(ScimMeta {
                resource_type: "Group".to_string(),
                ..Default::default()
            }),
        }
    }
}

/// SCIM 2.0 paginated list response envelope (RFC 7644).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScimListResponse<T> {
    pub schemas: Vec<String>,
    pub total_results: usize,
    pub items_per_page: usize,
    pub start_index: usize,
    pub resources: Vec<T>,
}

impl<T> ScimListResponse<T> {
    /// Creates a new SCIM list response envelope.
    pub fn new(resources: Vec<T>, total_results: usize, start_index: usize) -> Self {
        let items_per_page = resources.len();
        Self {
            schemas: vec![SCIM_LIST_SCHEMA.to_string()],
            total_results,
            items_per_page,
            start_index,
            resources,
        }
    }
}

/// SCIM 2.0 error response envelope (RFC 7644 Section 3.12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScimErrorResponse {
    pub schemas: Vec<String>,
    pub status: String,
    pub scim_type: Option<String>,
    pub detail: Option<String>,
}

impl ScimErrorResponse {
    /// Creates a new SCIM error.
    pub fn new(status: u16, scim_type: Option<&str>, detail: Option<&str>) -> Self {
        Self {
            schemas: vec![SCIM_ERROR_SCHEMA.to_string()],
            status: status.to_string(),
            scim_type: scim_type.map(str::to_string),
            detail: detail.map(str::to_string),
        }
    }
}
