//! This host's own MagicDNS name, for the loopback listener's allow-set (#9792).
//!
//! Why: `tailscale serve` proxies `https://<this node's MagicDNS name>/` to the
//! loopback listener and keeps the browser's `Host`, so that name is the
//! console's own authority too. It must never be hardcoded, and a failed
//! lookup must leave the allow-set loopback-only.
//! What: [`OwnMagicDns`], a set-once shared handle the loopback guard reads per
//! request; [`lookup_magic_dns`], which finds the name the way the tailnet
//! listener does — the host's tailnet IPv4 (`bind::detect_tailscale_ipv4`),
//! then `tailscale whois` on it (`PeerIdentity::node_name`); and
//! [`spawn_magic_dns_lookup`], the bounded background retry `run_serve` starts.
//! Test: `magic_dns_lookup_fails_closed_9792`,
//! `magic_dns_name_is_served_over_https_when_known_9792`,
//! `magic_dns_host_is_refused_when_the_name_is_unknown_9792`.

use std::net::IpAddr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use super::{PeerResolver, WHOIS_TIMEOUT};

/// How many times `run_serve` tries to learn the name; tailscaled may start
/// after the console at boot.
const LOOKUP_ATTEMPTS: u32 = 6;
/// The pause between two lookup attempts.
const LOOKUP_RETRY: Duration = Duration::from_secs(20);

/// This host's MagicDNS name, set at most once after startup.
///
/// What: cloned into every loopback listener's guard; empty until a lookup
/// records a valid name, which is the fail-closed state.
#[derive(Debug, Clone, Default)]
pub struct OwnMagicDns(Arc<OnceLock<String>>);

impl OwnMagicDns {
    /// The name, once a lookup has recorded one.
    pub fn get(&self) -> Option<&str> {
        self.0.get().map(String::as_str)
    }

    /// Record `name` when it is a plain DNS name; returns whether it was
    /// recorded. Only the first valid name is kept.
    ///
    /// Test: `magic_dns_lookup_fails_closed_9792`.
    pub fn set(&self, name: &str) -> bool {
        // #9792: an invalid name is refused, so it can never widen the allow-set.
        let Some(name) = valid_dns_name(name) else {
            return false;
        };
        self.0.set(name).is_ok()
    }
}

/// `name` normalized (lowercase, no trailing dot) when it is a multi-label DNS
/// name that is neither `localhost` nor an IP address; otherwise `None`.
fn valid_dns_name(name: &str) -> Option<String> {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    let label_ok = |label: &str| {
        (1..=63).contains(&label.len())
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    let plain = name.len() <= 253 && name.contains('.') && name.split('.').all(label_ok);
    (plain && name != "localhost" && name.parse::<IpAddr>().is_err()).then_some(name)
}

/// Look up this host's MagicDNS name.
///
/// Why: the tailnet listener learns its name from `tailscale whois` on its own
/// address (`TailnetPeerGate::host_node_name`); a loopback-only console has no
/// tailnet listener, so it finds that address with `detect` first.
/// What: runs `detect` on the blocking pool, then `resolver.whois` on the
/// address, each bounded by `bound`. Every failure — no address, a panic, a
/// timeout, a whois error, no node name, an invalid name — is `None`.
/// Test: `magic_dns_lookup_fails_closed_9792`.
pub async fn lookup_magic_dns<D>(
    detect: D,
    resolver: &dyn PeerResolver,
    bound: Duration,
) -> Option<String>
where
    D: FnOnce() -> Option<IpAddr> + Send + 'static,
{
    let ip = match tokio::time::timeout(bound, tokio::task::spawn_blocking(detect)).await {
        Ok(Ok(Some(ip))) => ip,
        _ => return None,
    };
    let identity = match tokio::time::timeout(bound, resolver.whois(ip)).await {
        Ok(Ok(identity)) => identity,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "MagicDNS name lookup failed");
            return None;
        }
        Err(_) => return None,
    };
    valid_dns_name(&identity.node_name?)
}

/// Learn the name in the background and record it in `handle`.
///
/// Why: startup must not wait on, or fail because of, tailscale.
/// What: up to [`LOOKUP_ATTEMPTS`] tries of [`lookup_magic_dns`],
/// [`LOOKUP_RETRY`] apart; the first name found is recorded and logged. When
/// none is found the allow-set stays loopback-only.
/// Test: the lookup and the guard are tested; this loop is not.
pub fn spawn_magic_dns_lookup(
    handle: OwnMagicDns,
    detect: fn() -> Option<IpAddr>,
    resolver: Arc<dyn PeerResolver>,
) {
    tokio::spawn(async move {
        for attempt in 1..=LOOKUP_ATTEMPTS {
            if let Some(name) = lookup_magic_dns(detect, resolver.as_ref(), WHOIS_TIMEOUT).await {
                if handle.set(&name) {
                    tracing::info!(name = %name, "loopback listener also answers to https://{name}");
                }
                return;
            }
            if attempt < LOOKUP_ATTEMPTS {
                tokio::time::sleep(LOOKUP_RETRY).await;
            }
        }
        tracing::info!("no MagicDNS name found; the loopback listener answers to loopback only");
    });
}
