//! Multi-tenant primitives: resolution, extractors, middleware, and isolated caching.

pub mod cache;
pub mod error;
pub mod layer;
pub mod model;
pub mod resolver;

pub use cache::TenantScopedCache;
pub use error::TenantError;
pub use layer::TenantLayer;
pub use model::{OptionalTenant, Tenant, TenantContext};
pub use resolver::{HeaderTenantResolver, SubdomainTenantResolver, TenantResolver};
