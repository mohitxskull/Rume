//! Shared SSRF protection and hardened `reqwest::Client` construction for auth HTTP clients.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use url::{Host, Url};

use crate::auth::error::AuthError;

pub(crate) const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn is_forbidden_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    let is_cgnat = octets[0] == 100 && (octets[1] & 0xc0) == 64;
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || is_cgnat
}

pub(crate) fn is_forbidden_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_forbidden_ipv4(v4);
    }
    let segments = ip.segments();
    let is_ula = (segments[0] & 0xfe00) == 0xfc00;
    let is_link_local = (segments[0] & 0xffc0) == 0xfe80;
    ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() || is_ula || is_link_local
}

pub(crate) fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_forbidden_ipv4(v4),
        IpAddr::V6(v6) => is_forbidden_ipv6(v6),
    }
}

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

    let Some(host) = parsed.host() else {
        return Err(AuthError::InvalidCredentials(format!(
            "{label} missing host"
        )));
    };

    if !allow_http {
        match host {
            Host::Ipv4(v4) if is_forbidden_ipv4(v4) => {
                return Err(AuthError::InvalidCredentials(format!(
                    "{label} blocked by SSRF protection: forbidden IP '{v4}'"
                )));
            }
            Host::Ipv6(v6) if is_forbidden_ipv6(v6) => {
                return Err(AuthError::InvalidCredentials(format!(
                    "{label} blocked by SSRF protection: forbidden IP '{v6}'"
                )));
            }
            Host::Domain(domain)
                if domain.eq_ignore_ascii_case("localhost")
                    || domain.to_ascii_lowercase().ends_with(".localhost") =>
            {
                return Err(AuthError::InvalidCredentials(format!(
                    "{label} blocked by SSRF protection: forbidden host '{domain}'"
                )));
            }
            _ => {}
        }
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
