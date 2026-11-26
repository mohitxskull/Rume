//! Service discovery abstractions for outbound HTTP calls.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;

use crate::client::OutboundHttpError;

/// Pinned boxed future type alias for async service discovery lookups.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Resolved target endpoint returned by [`ServiceDiscovery`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceEndpoint {
    /// Target host or IP address.
    pub host: String,
    /// Target port.
    pub port: u16,
    /// Priority (lower is preferred, per RFC 2782 SRV).
    pub priority: u16,
    /// Relative weight for load-balancing within the same priority tier.
    pub weight: u16,
}

/// Service discovery contract resolving logical service names to ordered endpoints.
pub trait ServiceDiscovery: Debug + Send + Sync + 'static {
    /// Resolves `service` into a list of candidate [`ServiceEndpoint`]s ordered by preference.
    fn resolve<'a>(
        &'a self,
        service: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ServiceEndpoint>, OutboundHttpError>>;
}

/// Static in-memory [`ServiceDiscovery`] mapping service names to fixed endpoints.
#[derive(Debug, Clone, Default)]
pub struct StaticDiscovery {
    _routes: BTreeMap<String, Vec<ServiceEndpoint>>,
}

impl StaticDiscovery {
    /// Creates an empty [`StaticDiscovery`] registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers endpoints for `service`.
    pub fn with_service(
        self,
        _service: impl Into<String>,
        _endpoints: Vec<ServiceEndpoint>,
    ) -> Self {
        todo!("Task 04: implement StaticDiscovery::with_service")
    }
}

impl ServiceDiscovery for StaticDiscovery {
    fn resolve<'a>(
        &'a self,
        _service: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ServiceEndpoint>, OutboundHttpError>> {
        todo!("Task 04: implement StaticDiscovery::resolve")
    }
}

/// DNS SRV [`ServiceDiscovery`] backed by `hickory-resolver`.
#[cfg(feature = "dns-srv")]
#[derive(Debug, Clone)]
pub struct DnsSrvDiscovery {
    _resolver: hickory_resolver::TokioResolver,
}

#[cfg(feature = "dns-srv")]
impl DnsSrvDiscovery {
    /// Creates a [`DnsSrvDiscovery`] using system DNS configuration.
    pub fn from_system_conf() -> Result<Self, OutboundHttpError> {
        todo!("Task 04: implement DnsSrvDiscovery::from_system_conf")
    }
}

#[cfg(feature = "dns-srv")]
impl ServiceDiscovery for DnsSrvDiscovery {
    fn resolve<'a>(
        &'a self,
        _service: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ServiceEndpoint>, OutboundHttpError>> {
        todo!("Task 04: implement DnsSrvDiscovery::resolve")
    }
}
