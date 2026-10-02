//! Tailnet peer identity lookup via `tailscale whois --json` (#9035).
//!
//! Why: the tailnet listener must know who owns the node on the other end of a
//! connection before it serves anything. `tailscale whois` is the tailnet's own
//! answer to that question, and the `tailscale` CLI is already a runtime
//! dependency of `--tailscale` mode (`bind::detect_tailscale_ipv4`).
//! What: [`PeerIdentity`], the [`PeerResolver`] seam the gate calls through
//! (injected in tests, so no real tailnet is needed), the production
//! [`TailscaleCliResolver`], and the pure [`parse_whois_json`] it delegates to.
//! Test: `parse_whois_*` in `tailnet_peer/tests.rs`.

use std::net::IpAddr;
use std::process::Stdio;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::Deserialize;

/// The login Tailscale reports for every tagged node, which has no human owner.
const TAGGED_DEVICES_LOGIN: &str = "tagged-devices";

/// A tailnet node's identity as `tailscale whois` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    /// The owning user's login name, e.g. `alice@example.com`.
    pub login: String,
    /// True when the node carries ACL tags; a tagged node has no human owner.
    pub tagged: bool,
}

/// Why an identity lookup produced no identity. Every variant fails closed.
#[derive(Debug, thiserror::Error)]
pub enum WhoisError {
    /// The `tailscale` binary could not be started.
    #[error("could not run `tailscale whois`: {0}")]
    Spawn(#[source] std::io::Error),
    /// `tailscale whois` ran and exited non-zero (daemon down, unknown peer).
    #[error("`tailscale whois` exited with {status}: {stderr}")]
    Exit {
        /// The exit status, rendered.
        status: String,
        /// The trimmed stderr text.
        stderr: String,
    },
    /// The output was not the expected JSON shape.
    #[error("could not parse `tailscale whois --json` output: {0}")]
    Parse(String),
    /// The output parsed but named no user login.
    #[error("`tailscale whois` reported no user login for the node")]
    NoLogin,
    /// The lookup did not finish inside the gate's time bound.
    #[error("identity lookup timed out after {0:?}")]
    Timeout(Duration),
}

/// Resolves the identity of the tailnet node holding an address.
///
/// Why: the gate's policy must be testable without a tailnet, so the lookup is a
/// seam rather than a hard-coded process spawn.
/// What: one method returning a boxed future, so the trait stays object-safe and
/// the gate can hold an `Arc<dyn PeerResolver>`.
/// Test: `tailnet_peer/tests.rs` injects scripted resolvers.
pub trait PeerResolver: Send + Sync + 'static {
    /// Look up the node identity behind `ip`.
    fn whois(&self, ip: IpAddr) -> BoxFuture<'_, Result<PeerIdentity, WhoisError>>;
}

/// The production resolver: runs `tailscale whois --json <ip>`.
///
/// Why: the CLI talks to the local tailscaled over its own socket, so this needs
/// no credentials and no LocalAPI client of our own.
/// What: spawns the CLI with `kill_on_drop`, so the gate's timeout dropping the
/// future also kills the child; parses stdout with [`parse_whois_json`].
/// Test: not unit-tested (needs a live tailnet); its parser is.
#[derive(Debug, Clone, Copy, Default)]
pub struct TailscaleCliResolver;

impl PeerResolver for TailscaleCliResolver {
    fn whois(&self, ip: IpAddr) -> BoxFuture<'_, Result<PeerIdentity, WhoisError>> {
        Box::pin(async move {
            let out = tokio::process::Command::new("tailscale")
                .args(["whois", "--json", &ip.to_string()])
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output()
                .await
                .map_err(WhoisError::Spawn)?;
            if !out.status.success() {
                return Err(WhoisError::Exit {
                    status: out.status.to_string(),
                    stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
                });
            }
            parse_whois_json(&out.stdout)
        })
    }
}

/// The subset of `tailscale whois --json` the gate reads.
#[derive(Deserialize)]
struct WhoisJson {
    #[serde(rename = "Node")]
    node: Option<WhoisNode>,
    #[serde(rename = "UserProfile")]
    user_profile: Option<WhoisUser>,
}

#[derive(Deserialize)]
struct WhoisNode {
    #[serde(rename = "Tags", default)]
    tags: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct WhoisUser {
    #[serde(rename = "LoginName", default)]
    login_name: String,
}

/// Parse `tailscale whois --json` output into a [`PeerIdentity`].
///
/// Why: kept pure so the shape the gate depends on is pinned by unit tests.
/// What: reads `UserProfile.LoginName` and `Node.Tags`. A node is tagged when it
/// has any tag or its login is `tagged-devices`. A missing or empty login is
/// [`WhoisError::NoLogin`], never an empty identity that could compare equal.
/// Test: `parse_whois_reads_login_and_untagged_node`,
/// `parse_whois_flags_tagged_node`, `parse_whois_rejects_missing_login`.
pub fn parse_whois_json(bytes: &[u8]) -> Result<PeerIdentity, WhoisError> {
    let parsed: WhoisJson =
        serde_json::from_slice(bytes).map_err(|e| WhoisError::Parse(e.to_string()))?;
    let login = parsed
        .user_profile
        .map(|u| u.login_name.trim().to_owned())
        .filter(|l| !l.is_empty())
        .ok_or(WhoisError::NoLogin)?;
    let has_tags = parsed
        .node
        .and_then(|n| n.tags)
        .is_some_and(|tags| !tags.is_empty());
    let tagged = has_tags || login == TAGGED_DEVICES_LOGIN;
    Ok(PeerIdentity { login, tagged })
}
