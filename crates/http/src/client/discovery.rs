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

/// Sorts [`ServiceEndpoint`] candidates by ascending SRV priority, then descending weight.
pub fn sort_srv_endpoints(endpoints: &mut [ServiceEndpoint]) {
    endpoints.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| b.weight.cmp(&a.weight))
    });
}

/// Static in-memory [`ServiceDiscovery`] mapping service names to fixed endpoints.
#[derive(Debug, Clone, Default)]
pub struct StaticDiscovery {
    routes: BTreeMap<String, Vec<ServiceEndpoint>>,
}

impl StaticDiscovery {
    /// Creates an empty [`StaticDiscovery`] registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers endpoints for `service`, ordered by SRV priority and weight.
    pub fn with_service(
        mut self,
        service: impl Into<String>,
        mut endpoints: Vec<ServiceEndpoint>,
    ) -> Self {
        sort_srv_endpoints(&mut endpoints);
        self.routes.insert(service.into(), endpoints);
        self
    }
}

impl ServiceDiscovery for StaticDiscovery {
    fn resolve<'a>(
        &'a self,
        service: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ServiceEndpoint>, OutboundHttpError>> {
        Box::pin(async move {
            match self.routes.get(service) {
                Some(endpoints) if !endpoints.is_empty() => Ok(endpoints.clone()),
                _ => Err(OutboundHttpError::Discovery(format!(
                    "no endpoints registered for service '{service}'"
                ))),
            }
        })
    }
}

/// DNS SRV [`ServiceDiscovery`] backed by `hickory-resolver`.
#[cfg(feature = "dns-srv")]
#[derive(Debug, Clone)]
pub struct DnsSrvDiscovery {
    resolver: hickory_resolver::TokioResolver,
}

#[cfg(feature = "dns-srv")]
impl DnsSrvDiscovery {
    /// Creates a [`DnsSrvDiscovery`] using system DNS configuration.
    pub fn from_system_conf() -> Result<Self, OutboundHttpError> {
        let resolver = hickory_resolver::TokioResolver::builder_tokio()
            .map_err(|err| OutboundHttpError::Discovery(err.to_string()))?
            .build()
            .map_err(|err| OutboundHttpError::Discovery(err.to_string()))?;
        Ok(Self { resolver })
    }
}

#[cfg(feature = "dns-srv")]
impl ServiceDiscovery for DnsSrvDiscovery {
    fn resolve<'a>(
        &'a self,
        service: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ServiceEndpoint>, OutboundHttpError>> {
        Box::pin(async move {
            let lookup = self
                .resolver
                .srv_lookup(service)
                .await
                .map_err(|err| OutboundHttpError::Discovery(err.to_string()))?;

            let mut endpoints: Vec<ServiceEndpoint> = lookup
                .answers()
                .iter()
                .filter_map(|record| match &record.data {
                    hickory_resolver::proto::rr::RData::SRV(srv) => Some(ServiceEndpoint {
                        host: srv.target.to_utf8().trim_end_matches('.').to_string(),
                        port: srv.port,
                        priority: srv.priority,
                        weight: srv.weight,
                    }),
                    _ => None,
                })
                .collect();

            if endpoints.is_empty() {
                return Err(OutboundHttpError::Discovery(format!(
                    "no SRV records found for '{service}'"
                )));
            }

            sort_srv_endpoints(&mut endpoints);
            Ok(endpoints)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn static_discovery_and_srv_selection_resolve_targets() {
        let discovery = StaticDiscovery::new().with_service(
            "orders.internal",
            vec![
                ServiceEndpoint {
                    host: "orders-b.internal".to_string(),
                    port: 8080,
                    priority: 20,
                    weight: 10,
                },
                ServiceEndpoint {
                    host: "orders-a1.internal".to_string(),
                    port: 8080,
                    priority: 10,
                    weight: 5,
                },
                ServiceEndpoint {
                    host: "orders-a2.internal".to_string(),
                    port: 8080,
                    priority: 10,
                    weight: 20,
                },
            ],
        );

        let resolved = discovery
            .resolve("orders.internal")
            .await
            .expect("resolve static service");
        assert_eq!(resolved.len(), 3);
        assert_eq!(resolved[0].host, "orders-a2.internal");
        assert_eq!(resolved[1].host, "orders-a1.internal");
        assert_eq!(resolved[2].host, "orders-b.internal");

        let missing = discovery.resolve("unknown.internal").await;
        assert!(matches!(missing, Err(OutboundHttpError::Discovery(_))));
    }
}
