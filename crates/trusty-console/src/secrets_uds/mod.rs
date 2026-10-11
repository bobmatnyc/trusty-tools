//! trusty-console's bridge to the trusty-secrets socket (#9066, DOC-74 §15.6).
//!
//! Why: the console's Secrets page lists and changes credentials, and the
//! secrets server answers only on its Unix socket. The browser cannot dial a
//! socket, so these routes translate each `POST /api/secrets/<verb>` into one
//! `secrets.bridge.<verb>` call, carrying the console's verdict on who the
//! HTTP client is (owner ruling Dd7 B). The console writes no audit of its
//! own; the server records every bridged call (ruling Ddf).
//! What: [`SecretsBridge`], the shared state — the dial, the per-launch CSRF
//! token, the console's own loopback authorities and the process table;
//! [`router`], the routes behind [`guard::guard`]; and [`TailnetRequest`],
//! the tag the tailnet listener puts on every request so the guard refuses it.
//! Test: `tests.rs` beside this module; `tests/secrets_uds_bridge.rs`.

pub mod dial;
pub mod errors;
pub mod guard;
pub mod pid_map;
pub mod routes;
pub mod verdict;

#[cfg(test)]
mod mount_tests;
#[cfg(test)]
mod picker_tests;
#[cfg(test)]
mod tests;

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use axum::Router;
use axum::routing::any;
use rand_core::{OsRng, RngCore};
use trusty_secrets::ancestry::{OsProcessTable, ProcessTable};

use crate::tailnet_peer::SelfAuthorities;
use dial::SecretsDial;

pub use crate::tailnet_peer::TailnetRequest;

/// The header every secrets route but `session` must carry the token in.
pub const CSRF_HEADER: &str = "x-trusty-secrets-csrf";
/// The largest request body a secrets route reads, in bytes.
pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// Why the bridge could not be built.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    /// `$HOME` is unknown, so the socket path is too.
    #[error("home directory is unavailable")]
    HomeUnavailable,
    /// The OS random source failed.
    #[error("the OS random source failed")]
    Random,
}

/// A 256-bit token minted once per console launch (A12).
///
/// What: compared in constant time; `Debug` never prints it.
#[derive(Clone)]
pub struct CsrfToken([u8; 32]);

impl CsrfToken {
    /// Mint a token from the OS CSPRNG.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Random`].
    pub fn mint() -> Result<Self, BridgeError> {
        let mut bytes = [0u8; 32];
        OsRng
            .try_fill_bytes(&mut bytes)
            .map_err(|_| BridgeError::Random)?;
        Ok(Self(bytes))
    }

    /// The token as 64 lowercase hex digits, the form the page sends back.
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Whether `candidate` is this token, in time independent of where it differs.
    ///
    /// What: a candidate of any other length is refused at once; its length
    /// is not secret.
    pub fn matches(&self, candidate: &str) -> bool {
        let expected = self.to_hex();
        if candidate.len() != expected.len() {
            return false;
        }
        expected
            .bytes()
            .zip(candidate.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }
}

impl fmt::Debug for CsrfToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CsrfToken(<redacted>)")
    }
}

/// The secrets bridge's shared state.
///
/// Why: one dial per console, so concurrent requests share the supervisor's
/// spawn gate; one token per launch.
/// What: `enabled` is `None` when the bridge could not be built — every
/// route then refuses. Authorities are set once the listeners are bound;
/// until then every request is refused (A11).
pub struct SecretsBridge {
    enabled: Option<(SecretsDial, CsrfToken)>,
    authorities: OnceLock<Vec<SelfAuthorities>>,
    table: Arc<dyn ProcessTable>,
    // #9067: the tm project registry file; `None` when `$HOME` is unknown.
    registry: Option<PathBuf>,
}

impl fmt::Debug for SecretsBridge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretsBridge")
            .field("enabled", &self.enabled.is_some())
            .finish_non_exhaustive()
    }
}

impl SecretsBridge {
    /// A bridge that refuses every request; `AppState::new` starts with one.
    pub fn disabled() -> Self {
        Self {
            enabled: None,
            authorities: OnceLock::new(),
            table: Arc::new(OsProcessTable),
            registry: None,
        }
    }

    /// A bridge dialling `dial`, with a freshly minted token.
    ///
    /// # Errors
    ///
    /// [`BridgeError::Random`].
    pub fn new(dial: SecretsDial) -> Result<Self, BridgeError> {
        Ok(Self {
            enabled: Some((dial, CsrfToken::mint()?)),
            ..Self::disabled()
        })
    }

    /// A bridge for `~/.trusty-tools/trusty-secrets/secrets.sock`.
    ///
    /// # Errors
    ///
    /// [`BridgeError`].
    pub fn for_home() -> Result<Self, BridgeError> {
        let home = dirs::home_dir().ok_or(BridgeError::HomeUnavailable)?;
        Self::new(SecretsDial::new(home.join(dial::SOCKET_SUBPATH)))
    }

    /// Judge clients against `table` instead of the OS (tests).
    #[must_use]
    pub fn with_table(mut self, table: Arc<dyn ProcessTable>) -> Self {
        self.table = table;
        self
    }

    /// Read the project picker from `registry` instead of tm's (tests).
    #[must_use]
    pub fn with_registry(mut self, registry: Option<PathBuf>) -> Self {
        self.registry = registry;
        self
    }

    /// Record the console's bound loopback addresses as its own authorities.
    ///
    /// What: only loopback addresses count; the tailnet listener is refused
    /// by its tag. Only the first call takes effect.
    pub fn set_authorities(&self, bound: &[SocketAddr]) {
        let own = bound
            .iter()
            .filter(|addr| addr.ip().is_loopback())
            .map(|addr| SelfAuthorities::new(*addr, None))
            .collect();
        // #9066: set once at startup; a second call is ignored.
        let _ = self.authorities.set(own);
    }

    pub(crate) fn authorities(&self) -> Option<&[SelfAuthorities]> {
        self.authorities.get().map(Vec::as_slice)
    }

    pub(crate) fn token(&self) -> Option<&CsrfToken> {
        self.enabled.as_ref().map(|(_, token)| token)
    }

    pub(crate) fn dial(&self) -> Option<&SecretsDial> {
        self.enabled.as_ref().map(|(dial, _)| dial)
    }

    pub(crate) fn table(&self) -> Arc<dyn ProcessTable> {
        Arc::clone(&self.table)
    }

    /// The socket the bridge dials, when enabled.
    pub fn socket(&self) -> Option<PathBuf> {
        self.dial().map(|dial| dial.socket().to_path_buf())
    }
}

impl crate::server::AppState {
    /// Serve the secrets routes through `bridge` (#9066).
    #[must_use]
    pub fn with_secrets(mut self, bridge: SecretsBridge) -> Self {
        self.secrets = Arc::new(bridge);
        self
    }

    /// The secrets bridge, shared with the router.
    pub fn secrets(&self) -> Arc<SecretsBridge> {
        Arc::clone(&self.secrets)
    }
}

/// Every `/api/secrets/*` route, behind the bridge's guard.
///
/// Why: mounted as its own sub-router, after the console's permissive CORS
/// layer and write-origin guard, so neither applies here (A6).
/// What: six verbs, `resolve` (501), and a catch-all (404) so no
/// `/api/secrets/*` path falls through to the generic proxy. Every route
/// takes any method; the guard answers everything but `POST` (Architect Q3).
/// Test: `unknown_secrets_paths_never_reach_the_socket`,
/// `the_real_router_sends_no_cors_header_on_secrets_routes`.
pub fn router(bridge: Arc<SecretsBridge>) -> Router {
    Router::new()
        .route("/api/secrets/session", any(routes::session))
        .route("/api/secrets/list", any(routes::list))
        .route("/api/secrets/set", any(routes::set))
        .route("/api/secrets/delete", any(routes::delete))
        .route("/api/secrets/copy", any(routes::copy))
        .route("/api/secrets/agents_may_use", any(routes::agents_may_use))
        .route("/api/secrets/resolve", any(routes::resolve))
        .route("/api/secrets", any(routes::unknown))
        .route("/api/secrets/", any(routes::unknown))
        .route("/api/secrets/{*rest}", any(routes::unknown))
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&bridge),
            guard::guard,
        ))
        .with_state(bridge)
}
