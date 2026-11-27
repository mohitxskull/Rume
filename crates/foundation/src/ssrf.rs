//! Canonical RFC 6890 SSRF IP and hostname validation.
//!
//! Pure `std::net` checks with zero extra dependencies, shared across outbound
//! HTTP surfaces (OAuth2/OIDC discovery, JWKS fetching, outbound webhooks).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Returns `true` if `ip` falls within any RFC 6890 special-purpose IPv4 range
/// that must not be targeted by server-side outbound requests.
///
/// Blocked ranges:
/// - `0.0.0.0/8` ("this network")
/// - `10.0.0.0/8` (RFC 1918 private)
/// - `100.64.0.0/10` (RFC 6598 CGNAT / shared address space)
/// - `127.0.0.0/8` (loopback)
/// - `169.254.0.0/16` (link-local / cloud metadata)
/// - `172.16.0.0/12` (RFC 1918 private)
/// - `192.0.0.0/24` (IETF protocol assignments)
/// - `192.0.2.0/24` (TEST-NET-1)
/// - `192.168.0.0/16` (RFC 1918 private)
/// - `198.18.0.0/15` (benchmarking)
/// - `198.51.100.0/24` (TEST-NET-2)
/// - `203.0.113.0/24` (TEST-NET-3)
/// - `224.0.0.0/4` (multicast)
/// - `240.0.0.0/4` (reserved / limited broadcast)
pub fn is_forbidden_ipv4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 0
        || o[0] == 10
        || (o[0] == 100 && (o[1] & 0xc0) == 64)
        || o[0] == 127
        || (o[0] == 169 && o[1] == 254)
        || (o[0] == 172 && (o[1] & 0xf0) == 16)
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        || (o[0] == 192 && o[1] == 0 && o[2] == 2)
        || (o[0] == 192 && o[1] == 168)
        || (o[0] == 198 && (o[1] & 0xfe) == 18)
        || (o[0] == 198 && o[1] == 51 && o[2] == 100)
        || (o[0] == 203 && o[1] == 0 && o[2] == 113)
        || (o[0] & 0xf0) == 224
        || (o[0] & 0xf0) == 240
}

/// Returns `true` if `ip` falls within any RFC 6890 special-purpose IPv6 range
/// that must not be targeted by server-side outbound requests.
///
/// Blocked ranges:
/// - `::/128` (unspecified)
/// - `::1/128` (loopback)
/// - `::ffff:0:0/96` (IPv4-mapped)
/// - `::/96` (deprecated IPv4-compatible)
/// - `64:ff9b::/96` (NAT64 well-known prefix)
/// - `64:ff9b:1::/48` (NAT64 local-use prefix)
/// - `100::/64` (discard-only)
/// - `2001::/32` (Teredo)
/// - `2001:db8::/32` (documentation)
/// - `2002::/16` (6to4)
/// - `fc00::/7` (unique local address / ULA)
/// - `fe80::/10` (link-local unicast)
/// - `fec0::/10` (deprecated site-local)
/// - `ff00::/8` (multicast)
pub fn is_forbidden_ipv6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    let is_ipv4_mapped = ip.to_ipv4_mapped().is_some();
    let is_ipv4_compatible = ip.to_ipv4().is_some();
    let is_nat64_well_known =
        s[0] == 0x0064 && s[1] == 0xff9b && s[2] == 0 && s[3] == 0 && s[4] == 0 && s[5] == 0;
    let is_nat64_local = s[0] == 0x0064 && s[1] == 0xff9b && s[2] == 0x0001;
    let is_discard = s[0] == 0x0100 && s[1] == 0 && s[2] == 0 && s[3] == 0;
    let is_teredo = s[0] == 0x2001 && s[1] == 0x0000;
    let is_documentation = s[0] == 0x2001 && s[1] == 0x0db8;
    let is_6to4 = s[0] == 0x2002;
    let is_ula = (s[0] & 0xfe00) == 0xfc00;
    let is_link_local = (s[0] & 0xffc0) == 0xfe80;
    let is_site_local = (s[0] & 0xffc0) == 0xfec0;
    let is_multicast = (s[0] & 0xff00) == 0xff00;

    ip.is_unspecified()
        || ip.is_loopback()
        || is_ipv4_mapped
        || is_ipv4_compatible
        || is_nat64_well_known
        || is_nat64_local
        || is_discard
        || is_teredo
        || is_documentation
        || is_6to4
        || is_ula
        || is_link_local
        || is_site_local
        || is_multicast
}

/// Returns `true` if `ip` is a forbidden RFC 6890 special-purpose IP address.
pub fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_forbidden_ipv4(v4),
        IpAddr::V6(v6) => is_forbidden_ipv6(v6),
    }
}

fn is_numeric_ipv4_label(part: &str) -> bool {
    if part.is_empty() {
        return false;
    }
    if let Some(hex) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
        return !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit());
    }
    part.bytes().all(|b| b.is_ascii_digit())
}

fn looks_like_non_canonical_ipv4(host: &str) -> bool {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return false;
    }
    parts.iter().all(|p| is_numeric_ipv4_label(p))
}

/// Validates a URL host string (domain name or IP literal, optionally bracketed
/// for IPv6) against SSRF rules.
///
/// Rejects empty hosts, IPv6 zone IDs (`%`), invalid bracketed hosts,
/// non-canonical IPv4 shorthands (decimal/hex/octal/short forms), RFC 6890
/// special-purpose IP literals, and reserved local/internal hostnames
/// (`localhost`, `*.localhost`, `*.local`, `*.internal`) after stripping any
/// trailing DNS root dot(s).
pub fn validate_host(host: &str) -> Result<(), String> {
    let trimmed = host.trim();
    if trimmed.is_empty() {
        return Err("missing host".to_owned());
    }

    let normalized = trimmed.trim_end_matches('.');
    if normalized.is_empty() {
        return Err(format!("forbidden host '{host}'"));
    }

    // Why: reject IPv6 zone IDs (e.g. `[fe80::1%eth0]`, `[::1%lo0]`) because
    // `std::net::IpAddr::from_str` rejects `%` and would otherwise fall through
    // to the hostname branch (`RC-36`).
    if normalized.contains('%') {
        return Err(format!("forbidden IPv6 zone ID in host '{host}'"));
    }

    if normalized.contains('[') || normalized.contains(']') {
        let Some(inner) = normalized
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
        else {
            return Err(format!("invalid bracketed host '{host}'"));
        };
        let Ok(v6) = inner.parse::<Ipv6Addr>() else {
            return Err(format!("invalid bracketed IPv6 host '{host}'"));
        };
        if is_forbidden_ipv6(v6) {
            return Err(format!("forbidden IP '{v6}'"));
        }
        return Ok(());
    }

    if let Ok(ip) = normalized.parse::<IpAddr>() {
        if is_forbidden_ip(ip) {
            return Err(format!("forbidden IP '{ip}'"));
        }
        return Ok(());
    }

    // Why: OS resolvers (`getaddrinfo`) accept non-canonical IPv4 forms such as
    // decimal integers (`2130706433`), hex (`0x7f000001`), octal (`0177.0.0.1`),
    // and short dotted forms (`127.1`) that fail `Ipv4Addr::from_str` (`RC-36`).
    if looks_like_non_canonical_ipv4(normalized) {
        return Err(format!("forbidden non-canonical IPv4 host '{host}'"));
    }

    let lower = normalized.to_ascii_lowercase();
    if lower == "localhost"
        || lower.ends_with(".localhost")
        || lower.ends_with(".local")
        || lower.ends_with(".internal")
    {
        return Err(format!("forbidden host '{host}'"));
    }

    Ok(())
}

/// Returns `true` if `host` is rejected by [`validate_host`].
pub fn is_forbidden_host(host: &str) -> bool {
    validate_host(host).is_err()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_all_rfc6890_ipv4_special_ranges() {
        let blocked = [
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.1",
            "100.64.0.1",
            "100.127.255.254",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "172.31.255.254",
            "192.0.0.1",
            "192.0.2.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.19.255.254",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "239.255.255.255",
            "240.0.0.1",
            "255.255.255.255",
        ];
        for raw in blocked {
            let ip: Ipv4Addr = raw.parse().unwrap();
            assert!(is_forbidden_ipv4(ip), "expected IPv4 {raw} to be blocked");
            assert!(is_forbidden_ip(IpAddr::V4(ip)));
            assert!(is_forbidden_host(raw));
        }

        let allowed = [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.216.34",
            "172.32.0.1",
            "198.20.0.1",
        ];
        for raw in allowed {
            let ip: Ipv4Addr = raw.parse().unwrap();
            assert!(
                !is_forbidden_ipv4(ip),
                "expected public IPv4 {raw} to be allowed"
            );
            assert!(validate_host(raw).is_ok());
        }
    }

    #[test]
    fn blocks_all_rfc6890_ipv6_special_ranges() {
        let blocked = [
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:8.8.8.8",
            "::127.0.0.1",
            "::169.254.169.254",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b::7f00:1",
            "64:ff9b:1::1",
            "100::1",
            "2001::1",
            "2001:db8::1",
            "2002:7f00:1::1",
            "fc00::1",
            "fd00::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
        ];
        for raw in blocked {
            let ip: Ipv6Addr = raw.parse().unwrap();
            assert!(is_forbidden_ipv6(ip), "expected IPv6 {raw} to be blocked");
            assert!(is_forbidden_ip(IpAddr::V6(ip)));
            assert!(is_forbidden_host(raw));
            assert!(is_forbidden_host(&format!("[{raw}]")));
        }

        let allowed = ["2606:2800:220:1:248:1893:25c8:1946", "2001:4860:4860::8888"];
        for raw in allowed {
            let ip: Ipv6Addr = raw.parse().unwrap();
            assert!(
                !is_forbidden_ipv6(ip),
                "expected public IPv6 {raw} to be allowed"
            );
            assert!(validate_host(raw).is_ok());
            assert!(validate_host(&format!("[{raw}]")).is_ok());
        }
    }

    #[test]
    fn validates_hostnames_with_trailing_dots_and_reserved_suffixes() {
        for blocked in [
            "",
            ".",
            "localhost",
            "localhost.",
            "LOCALHOST.",
            "sub.localhost",
            "sub.localhost.",
            "printer.local",
            "printer.local.",
            "metadata.google.internal",
            "metadata.google.internal.",
        ] {
            assert!(
                is_forbidden_host(blocked),
                "expected host {blocked:?} to be blocked"
            );
        }

        for allowed in ["example.com", "api.github.com", "idp.example.com."] {
            assert!(
                validate_host(allowed).is_ok(),
                "expected host {allowed:?} to be allowed"
            );
        }
    }

    #[test]
    fn blocks_ipv6_zone_ids_invalid_brackets_and_non_canonical_ipv4() {
        for blocked in [
            "[fe80::1%eth0]",
            "[::1%lo0]",
            "fe80::1%eth0",
            "[not-an-ip]",
            "[127.0.0.1]",
            "foo[bar]",
            "2130706433",
            "0x7f000001",
            "0X7F000001",
            "0177.0.0.1",
            "127.1",
            "127.0.1",
            "0x7f.0.0.1",
        ] {
            assert!(
                is_forbidden_host(blocked),
                "expected host {blocked:?} to be blocked"
            );
        }
    }
}
