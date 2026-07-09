//! IP -> cloud-provider classification for the egress-topology feature.
//!
//! Attribution works off each provider's *published* prefix list. We keep a
//! small, always-available hardcoded fallback table (Cloudflare's list is exact;
//! AWS/GCP/Azure fallbacks are coarse owned-aggregates) so the classifier is
//! never empty, and asynchronously upgrade it to the full live prefix lists
//! (AWS `ip-ranges.json`, GCP `cloud.json`) fetched in the background.
//!
//! Design notes:
//! * Private / loopback / link-local addresses are excluded *before* any cloud
//!   lookup and reported as `Private` — they are never egress.
//! * `100.64.0.0/10` (RFC 6598) is treated as Tailscale / mesh. It is also ISP
//!   carrier-grade NAT, so it is labelled "mesh/CGNAT", never a public cloud.
//! * Matching is longest-prefix-wins so a specific service prefix beats the
//!   provider's catch-all supernet.

use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::RwLock;

/// Which network a destination IP belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    Aws,
    Azure,
    Gcp,
    Cloudflare,
    /// Tailscale / RFC 6598 CGNAT mesh — reachable but not a metered public cloud.
    Tailscale,
    /// RFC1918 / loopback / link-local — local, never egress.
    Private,
    /// Publicly routable but not matched to a known cloud.
    Unknown,
}

impl Provider {
    /// Display string consumed by the frontend egress table.
    pub fn label(self) -> &'static str {
        match self {
            Provider::Aws => "AWS",
            Provider::Azure => "Azure",
            Provider::Gcp => "GCP",
            Provider::Cloudflare => "Cloudflare",
            Provider::Tailscale => "Tailscale",
            Provider::Private => "Local",
            Provider::Unknown => "Internet",
        }
    }

    /// Rough egress price in USD per GB, or `None` when traffic is unmetered
    /// (mesh / local) or the provider is unknown (we refuse to invent a price).
    pub fn egress_usd_per_gb(self) -> Option<f64> {
        match self {
            // Coarse public list-price ballpark for standard internet egress.
            Provider::Aws => Some(0.09),
            Provider::Azure => Some(0.087),
            Provider::Gcp => Some(0.12),
            // CDN / mesh / local: not standard per-GB egress we can attribute.
            Provider::Cloudflare | Provider::Tailscale | Provider::Private | Provider::Unknown => {
                None
            }
        }
    }

    pub fn is_mesh(self) -> bool {
        matches!(self, Provider::Tailscale)
    }
}

/// Result of classifying a single IP.
#[derive(Clone, Debug)]
pub struct Classification {
    pub provider: Provider,
    /// Human detail, e.g. "AWS (us-east-1 / EC2)" or "Cloudflare".
    pub detail: String,
}

/// A loaded set of provider prefixes plus where it came from.
pub struct Ranges {
    entries: Vec<(IpNet, Provider, String)>,
    /// "fallback" (hardcoded) or "live" (fetched published lists).
    pub source: &'static str,
}

impl Ranges {
    /// Longest-prefix-match classify. Assumes the caller already excluded
    /// private/loopback/link-local and CGNAT.
    fn match_public(&self, ip: IpAddr) -> Option<(Provider, String)> {
        let mut best: Option<(&IpNet, Provider, &String)> = None;
        for (net, provider, detail) in &self.entries {
            if net.contains(&ip) {
                let better = match best {
                    None => true,
                    Some((b, _, _)) => net.prefix_len() > b.prefix_len(),
                };
                if better {
                    best = Some((net, *provider, detail));
                }
            }
        }
        best.map(|(_, p, d)| (p, d.clone()))
    }
}

static RANGES: RwLock<Option<Ranges>> = RwLock::new(None);

/// Returns true if the address must never be treated as egress to a cloud.
pub fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || is_shared_cgnat(v4) // 100.64/10 handled separately as mesh
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || is_ipv6_link_local(v6)
                || is_ipv6_unique_local(v6)
        }
    }
}

/// RFC 6598 shared address space (100.64.0.0/10) — Tailscale mesh or ISP CGNAT.
fn is_shared_cgnat(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    o[0] == 100 && (64..=127).contains(&o[1])
}

fn is_ipv6_link_local(v6: Ipv6Addr) -> bool {
    (v6.segments()[0] & 0xffc0) == 0xfe80 // fe80::/10
}

fn is_ipv6_unique_local(v6: Ipv6Addr) -> bool {
    (v6.segments()[0] & 0xfe00) == 0xfc00 // fc00::/7
}

/// Classify a destination IP. Order: local exclusion -> CGNAT/mesh -> published
/// cloud prefixes -> unknown-but-public. Never panics; always returns something.
pub fn classify(ip: IpAddr) -> Classification {
    if let IpAddr::V4(v4) = ip {
        if is_shared_cgnat(v4) {
            return Classification {
                provider: Provider::Tailscale,
                detail: "Tailscale / CGNAT mesh (100.64.0.0/10)".to_string(),
            };
        }
    }
    if is_local(ip) {
        return Classification {
            provider: Provider::Private,
            detail: "Private / loopback".to_string(),
        };
    }

    // Ensure the table is at least populated with the fallback set.
    ensure_initialized();
    let guard = RANGES.read().unwrap();
    if let Some(ranges) = guard.as_ref() {
        if let Some((provider, detail)) = ranges.match_public(ip) {
            return Classification { provider, detail };
        }
    }
    Classification {
        provider: Provider::Unknown,
        detail: "Public internet (unattributed)".to_string(),
    }
}

/// Whether the currently loaded table is the coarse fallback set.
pub fn is_fallback() -> bool {
    ensure_initialized();
    RANGES
        .read()
        .unwrap()
        .as_ref()
        .map(|r| r.source == "fallback")
        .unwrap_or(true)
}

fn ensure_initialized() {
    let need = RANGES.read().unwrap().is_none();
    if need {
        let mut guard = RANGES.write().unwrap();
        if guard.is_none() {
            *guard = Some(build_fallback());
        }
    }
}

/// Coarse, always-available hardcoded table. Cloudflare's list is exact and
/// complete; AWS/GCP/Azure are broad owned aggregates chosen to minimise false
/// positives until the live lists load. Every hit here is low-confidence.
fn build_fallback() -> Ranges {
    let mut entries: Vec<(IpNet, Provider, String)> = Vec::new();
    let mut add = |cidr: &str, p: Provider, detail: &str| {
        if let Ok(net) = cidr.parse::<IpNet>() {
            entries.push((net, p, detail.to_string()));
        }
    };

    // Cloudflare — exact & complete (fallback is authoritative for CF).
    for cidr in [
        "173.245.48.0/20", "103.21.244.0/22", "103.22.200.0/22", "103.31.4.0/22",
        "141.101.64.0/18", "108.162.192.0/18", "190.93.240.0/20", "188.114.96.0/20",
        "197.234.240.0/22", "198.41.128.0/17", "162.158.0.0/15", "104.16.0.0/13",
        "104.24.0.0/14", "172.64.0.0/13", "131.0.72.0/22",
    ] {
        add(cidr, Provider::Cloudflare, "Cloudflare (fallback)");
    }
    for cidr in [
        "2400:cb00::/32", "2606:4700::/32", "2803:f800::/32", "2405:b500::/32",
        "2405:8100::/32", "2a06:98c0::/29", "2c0f:f248::/32",
    ] {
        add(cidr, Provider::Cloudflare, "Cloudflare (fallback)");
    }

    // AWS — coarse owned aggregates (not exhaustive).
    for cidr in ["52.0.0.0/8", "54.0.0.0/8", "3.0.0.0/8", "18.0.0.0/8", "35.152.0.0/13"] {
        add(cidr, Provider::Aws, "AWS (fallback aggregate)");
    }
    // GCP — coarse.
    for cidr in ["34.0.0.0/8", "35.192.0.0/11", "35.184.0.0/13"] {
        add(cidr, Provider::Gcp, "GCP (fallback aggregate)");
    }
    // Azure — coarse.
    for cidr in ["20.0.0.0/8", "40.64.0.0/10", "13.64.0.0/11"] {
        add(cidr, Provider::Azure, "Azure (fallback aggregate)");
    }

    Ranges { entries, source: "fallback" }
}

// --- Live prefix fetch (AWS + GCP) --------------------------------------------

#[derive(Deserialize)]
struct AwsRanges {
    prefixes: Vec<AwsPrefix>,
    #[serde(default)]
    ipv6_prefixes: Vec<AwsPrefix6>,
}
#[derive(Deserialize)]
struct AwsPrefix {
    ip_prefix: String,
    #[serde(default)]
    region: String,
    #[serde(default)]
    service: String,
}
#[derive(Deserialize)]
struct AwsPrefix6 {
    ipv6_prefix: String,
    #[serde(default)]
    region: String,
    #[serde(default)]
    service: String,
}

#[derive(Deserialize)]
struct GcpRanges {
    prefixes: Vec<GcpPrefix>,
}
#[derive(Deserialize)]
struct GcpPrefix {
    #[serde(default)]
    ipv4Prefix: String,
    #[serde(default)]
    ipv6Prefix: String,
    #[serde(default)]
    scope: String,
}

/// Fetch the live AWS + GCP prefix lists and swap them into the global table.
/// Best-effort: on any failure the existing (fallback or previous live) table is
/// left untouched. Intended to be spawned on a background task at startup.
pub async fn refresh_from_network(client: &reqwest::Client) {
    let mut entries: Vec<(IpNet, Provider, String)> = build_fallback().entries; // keep Cloudflare etc.

    // AWS
    if let Ok(resp) = client
        .get("https://ip-ranges.amazonaws.com/ip-ranges.json")
        .send()
        .await
    {
        if let Ok(aws) = resp.json::<AwsRanges>().await {
            for p in aws.prefixes {
                if let Ok(net) = p.ip_prefix.parse::<IpNet>() {
                    entries.push((net, Provider::Aws, aws_detail(&p.service, &p.region)));
                }
            }
            for p in aws.ipv6_prefixes {
                if let Ok(net) = p.ipv6_prefix.parse::<IpNet>() {
                    entries.push((net, Provider::Aws, aws_detail(&p.service, &p.region)));
                }
            }
        }
    }

    // GCP
    if let Ok(resp) = client
        .get("https://www.gstatic.com/ipranges/cloud.json")
        .send()
        .await
    {
        if let Ok(gcp) = resp.json::<GcpRanges>().await {
            for p in gcp.prefixes {
                let cidr = if !p.ipv4Prefix.is_empty() {
                    &p.ipv4Prefix
                } else {
                    &p.ipv6Prefix
                };
                if let Ok(net) = cidr.parse::<IpNet>() {
                    let detail = if p.scope.is_empty() {
                        "GCP".to_string()
                    } else {
                        format!("GCP ({})", p.scope)
                    };
                    entries.push((net, Provider::Gcp, detail));
                }
            }
        }
    }

    // Only publish "live" if we actually gained substantially more than the
    // fallback (i.e. at least one live fetch succeeded).
    let fallback_len = build_fallback().entries.len();
    if entries.len() > fallback_len {
        let mut guard = RANGES.write().unwrap();
        *guard = Some(Ranges { entries, source: "live" });
    }
}

fn aws_detail(service: &str, region: &str) -> String {
    match (service.is_empty(), region.is_empty()) {
        (false, false) => format!("AWS ({region} / {service})"),
        (false, true) => format!("AWS ({service})"),
        (true, false) => format!("AWS ({region})"),
        (true, true) => "AWS".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn excludes_private_and_loopback() {
        for a in ["127.0.0.1", "10.1.2.3", "192.168.1.1", "172.16.5.5", "169.254.1.1"] {
            let c = classify(ip(a));
            assert_eq!(c.provider, Provider::Private, "{a} should be Private");
        }
        assert_eq!(classify(ip("::1")).provider, Provider::Private);
        assert_eq!(classify(ip("fe80::1")).provider, Provider::Private);
        assert_eq!(classify(ip("fc00::1")).provider, Provider::Private);
    }

    #[test]
    fn cgnat_is_tailscale_mesh() {
        let c = classify(ip("100.112.55.21"));
        assert_eq!(c.provider, Provider::Tailscale);
        assert!(c.provider.is_mesh());
        assert!(c.provider.egress_usd_per_gb().is_none()); // mesh is free
    }

    #[test]
    fn cloudflare_matches_from_fallback() {
        // 104.16.0.0/13 is Cloudflare.
        assert_eq!(classify(ip("104.16.132.229")).provider, Provider::Cloudflare);
    }

    #[test]
    fn unknown_public_ip_is_unattributed_not_faked() {
        // A public IP outside every fallback aggregate must be Unknown, never
        // invented as a specific cloud.
        let c = classify(ip("198.51.100.7")); // TEST-NET-2, public-ish, unlisted
        assert_eq!(c.provider, Provider::Unknown);
        assert!(c.provider.egress_usd_per_gb().is_none());
    }

    #[test]
    fn longest_prefix_wins() {
        let mut entries: Vec<(IpNet, Provider, String)> = Vec::new();
        entries.push(("52.0.0.0/8".parse().unwrap(), Provider::Aws, "AWS supernet".into()));
        entries.push(("52.94.0.0/22".parse().unwrap(), Provider::Aws, "AWS specific".into()));
        let r = Ranges { entries, source: "test" };
        let (_, detail) = r.match_public(ip("52.94.0.5")).unwrap();
        assert_eq!(detail, "AWS specific");
    }
}
