//! Enterprise protocol starters: SCIM 2.0 provisioning and SAML 2.0 assertion validation.

pub mod saml;
pub mod scim;

pub use saml::{SamlAssertion, SamlError, SamlParser, SamlSignatureVerifier};
pub use scim::{
    SCIM_ERROR_SCHEMA, SCIM_GROUP_SCHEMA, SCIM_LIST_SCHEMA, SCIM_USER_SCHEMA, ScimEmail,
    ScimErrorResponse, ScimGroup, ScimListResponse, ScimMember, ScimMeta, ScimName, ScimUser,
};
