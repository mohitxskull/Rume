//! Security hardening primitives: headers, CSRF protection, and structured audit logs.

pub mod audit;
pub mod csrf;
pub mod headers;

pub use audit::{AuditError, AuditLogger, AuditRecord, AuditStatus, MemoryAuditLogger};
pub use csrf::{CsrfConfig, CsrfLayer, CsrfService};
pub use headers::{SecurityHeadersConfig, SecurityHeadersLayer, SecurityHeadersService};
