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
    #[serde(default)]
    pub primary: bool,
    #[serde(rename = "type", default)]
    pub email_type: Option<String>,
}

/// SCIM Role multi-valued attribute (RFC 7643 §4.1.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ScimRole {
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub display: Option<String>,
    #[serde(rename = "type", default)]
    pub role_type: Option<String>,
    #[serde(default)]
    pub primary: bool,
}

impl From<&str> for ScimRole {
    fn from(value: &str) -> Self {
        Self {
            value: Some(value.to_string()),
            ..Self::default()
        }
    }
}

impl From<String> for ScimRole {
    fn from(value: String) -> Self {
        Self {
            value: Some(value),
            ..Self::default()
        }
    }
}

fn default_true() -> bool {
    true
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
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub external_id: Option<String>,
    pub user_name: String,
    #[serde(default)]
    pub name: Option<ScimName>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default = "default_true")]
    pub active: bool,
    #[serde(default)]
    pub emails: Vec<ScimEmail>,
    #[serde(default)]
    pub roles: Vec<ScimRole>,
    #[serde(default)]
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
    #[serde(rename = "Resources")]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scim_role_accepts_display_only_and_string_conversions() {
        let role: ScimRole = serde_json::from_str(r#"{"display":"Administrator"}"#)
            .expect("RFC 7643 display-only role must deserialize");
        assert_eq!(role.value, None);
        assert_eq!(role.display.as_deref(), Some("Administrator"));

        let from_str: ScimRole = "admin".into();
        assert_eq!(from_str.value.as_deref(), Some("admin"));
    }

    #[test]
    fn scim_user_requires_schemas_field() {
        let missing = serde_json::from_str::<ScimUser>(r#"{"id":"u1","userName":"alice"}"#);
        assert!(
            missing.is_err(),
            "RFC 7643 requires `schemas` on ScimUser deserialization"
        );

        let valid = serde_json::from_str::<ScimUser>(
            r#"{"schemas":["urn:ietf:params:scim:schemas:core:2.0:User"],"userName":"alice"}"#,
        )
        .expect("schema-bearing ScimUser must deserialize");
        assert_eq!(valid.schemas, vec![SCIM_USER_SCHEMA.to_string()]);
        assert_eq!(valid.user_name, "alice");
    }
}
