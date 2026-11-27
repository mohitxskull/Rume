//! Shared SSRF protection and hardened `reqwest::Client` construction for auth HTTP clients.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use rume_foundation::ssrf::{is_forbidden_ip, validate_host};
use url::Url;

use crate::auth::error::AuthError;

pub(crate) const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn validate_outbound_url(
    parsed: &Url,
    allow_http: bool,
    label: &str,
) -> Result<(), AuthError> {
    if allow_http {
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(AuthError::InvalidCredentials(format!(
                "{label} must use http or https"
            )));
        }
    } else if parsed.scheme() != "https" {
        return Err(AuthError::InvalidCredentials(format!(
            "{label} must use HTTPS"
        )));
    }

    let Some(host) = parsed.host_str() else {
        return Err(AuthError::InvalidCredentials(format!(
            "{label} missing host"
        )));
    };

    if !allow_http {
        validate_host(host).map_err(|reason| {
            AuthError::InvalidCredentials(format!("{label} blocked by SSRF protection: {reason}"))
        })?;
    }

    Ok(())
}

#[derive(Debug)]
struct SsrfSafeDnsResolver;

impl reqwest::dns::Resolve for SsrfSafeDnsResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
                .collect();
            if addrs.is_empty() {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::AddrNotAvailable,
                    format!("DNS resolution returned no addresses for '{host}'"),
                ))
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            for addr in &addrs {
                if is_forbidden_ip(addr.ip()) {
                    return Err(Box::new(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!(
                            "SSRF protection blocked resolved private/internal IP {} for '{host}'",
                            addr.ip()
                        ),
                    ))
                        as Box<dyn std::error::Error + Send + Sync>);
                }
            }
            let iter: reqwest::dns::Addrs = Box::new(addrs.into_iter());
            Ok(iter)
        })
    }
}

pub(crate) fn build_hardened_client(
    timeout: Duration,
    allow_http: bool,
) -> Result<reqwest::Client, AuthError> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
        .timeout(timeout);
    if !allow_http {
        builder = builder.dns_resolver(Arc::new(SsrfSafeDnsResolver));
    }
    builder
        .build()
        .map_err(|e| AuthError::ProviderError(format!("HTTP client error: {e}")))
}
