//! Enterprise protocol starters: SCIM 2.0 provisioning and SAML 2.0 assertion validation.

pub mod scim;

#[cfg(feature = "saml")]
pub use crate::saml::{
    InMemorySamlReplayCache, SamlAssertion, SamlError, SamlParser, SamlReplayCache,
    SamlSignatureVerifier, SamlSubjectConfirmationData, SamlValidationConfig, parse_iso8601_strict,
};
pub use scim::{
    SCIM_ERROR_SCHEMA, SCIM_GROUP_SCHEMA, SCIM_LIST_SCHEMA, SCIM_USER_SCHEMA, ScimEmail,
    ScimErrorResponse, ScimGroup, ScimListResponse, ScimMember, ScimMeta, ScimName, ScimRole,
    ScimUser,
};
