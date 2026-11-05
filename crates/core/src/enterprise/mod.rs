//! Enterprise protocol starters: SCIM 2.0 provisioning and SAML 2.0 assertion validation.

#[cfg(feature = "saml")]
pub mod saml;
pub mod scim;

#[cfg(feature = "saml")]
pub use saml::{
    parse_iso8601_strict, SamlAssertion, SamlError, SamlParser, SamlSignatureVerifier,
};
pub use scim::{
    SCIM_ERROR_SCHEMA, SCIM_GROUP_SCHEMA, SCIM_LIST_SCHEMA, SCIM_USER_SCHEMA, ScimEmail,
    ScimErrorResponse, ScimGroup, ScimListResponse, ScimMember, ScimMeta, ScimName, ScimUser,
};
