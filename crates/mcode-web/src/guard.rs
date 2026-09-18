//! URL and response guards for outbound web requests.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Maximum accepted response body bytes per request.
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// Maximum one page body may contribute to the aggregate contents payload.
pub const MAX_PAGE_BYTES: usize = 1024 * 1024;
/// Maximum results accepted from one search response.
pub const MAX_SEARCH_RESULTS: usize = 32;
/// Maximum URLs accepted in one contents request.
pub const MAX_CONTENTS_URLS: usize = 8;
/// Default outbound timeout applied by the caller.
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Rejects URLs the web client must not fetch.
///
/// Only `https://` URLs with a host that is neither an IP literal in a
/// private, loopback, link-local, or otherwise non-public range nor a
/// trivially local name are accepted. Port numbers must be empty or the
/// default 443 so guards cannot be routed around through odd ports.
#[must_use]
pub fn is_fetchable_url(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("https://") else {
        return false;
    };
    if value.len() > 2048 || rest.is_empty() {
        return false;
    }
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    // Split host and port; bracketed IPv6 literals may carry "]":port".
    let (host, port) = if host_port.ends_with(']') {
        (host_port, None)
    } else if let Some((prefix, port)) = host_port.rsplit_once("]:") {
        (&host_port[..prefix.len() + 1], Some(port))
    } else {
        match host_port.rsplit_once(':') {
            Some((prefix, port)) if !prefix.is_empty() => (prefix, Some(port)),
            _ => (host_port, None),
        }
    };
    if let Some(port) = port
        && !port.is_empty()
        && port != "443"
    {
        return false;
    }
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let host = host.strip_prefix('[').unwrap_or(host);
    let host = host.strip_suffix(']').unwrap_or(host);
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    let lower = host.to_ascii_lowercase();
    if matches!(lower.as_str(), "localhost" | "localhost.localdomain") || lower.ends_with(".local")
    {
        return false;
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return is_public_ip(ip);
    }
    // Plain hostnames: require a plausible DNS name without whitespace or
    // scheme smuggling.
    host.bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'.')
        && !host.starts_with('-')
        && !host.contains("..")
}

/// Reports whether the address is globally routable public Internet space.
#[must_use]
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_ipv4(ip),
        IpAddr::V6(ip) => is_public_ipv6(ip),
    }
}

fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    if ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_unspecified()
        || ip.is_documentation()
    {
        return false;
    }
    // 0.0.0.0/8, 100.64.0.0/10 (CGNAT), 192.0.0.0/24, 198.18.0.0/15,
    // 240.0.0.0/4 reserved blocks.
    !(octets[0] == 0
        || (octets[0] == 100 && (octets[1] & 0b1100_0000) == 64)
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        || (octets[0] == 198 && (octets[1] & 0xfe) == 18)
        || octets[0] >= 240)
}

fn is_public_ipv6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return false;
    }
    let segments = ip.segments();
    // IPv4-mapped and IPv4-compatible addresses fall back to v4 rules.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_ipv4(v4);
    }
    // Unique-local fc00::/7, link-local fe80::/10, and the documentation,
    // discard-only, and Teredo prefixes are not public.
    !(segments[0] & 0xfe00 == 0xfc00
        || segments[0] & 0xffc0 == 0xfe80
        || segments[0] & 0xfffe == 0xfdfe
        || segments[0] == 0x2001 && segments[1] == 0x0db8
        || segments[0] == 0x100
        || segments[0] == 0x2001 && (segments[1] & 0xfff0) == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_public_https_urls() {
        for url in [
            "https://example.com/v1/search",
            "https://api.search.example.com:443/v1/search",
            "https://search.example.com",
        ] {
            assert!(is_fetchable_url(url), "{url}");
        }
    }

    #[test]
    fn rejects_schemes_and_local_hosts() {
        for url in [
            "http://example.com/v1/search",
            "ftp://example.com",
            "https://localhost/v1/search",
            "https://box.local/search",
            "https://127.0.0.1/v1/search",
            "https://10.1.2.3/v1/search",
            "https://192.168.1.1/v1/search",
            "https://172.16.0.9/v1/search",
            "https://169.254.1.1/v1/search",
            "https://[::1]/v1/search",
            "https://[fe80::1]/v1/search",
            "https://[fc00::1]/v1/search",
            "https://100.64.0.1/v1/search",
            "https://example.com:8443/v1/search",
            "https://",
            "https:///path",
        ] {
            assert!(!is_fetchable_url(url), "{url}");
        }
    }

    #[test]
    fn rejects_oversized_and_malformed_hosts() {
        let long = format!("https://{}.com/path", "a".repeat(300));
        assert!(!is_fetchable_url(&long));
        assert!(is_fetchable_url(&format!(
            "https://{}.com",
            "a".repeat(240)
        )));
        assert!(!is_fetchable_url("https://bad host/path"));
        assert!(!is_fetchable_url("https://..com/path"));
        assert!(!is_fetchable_url("https://-evil.com/path"));
    }
}
