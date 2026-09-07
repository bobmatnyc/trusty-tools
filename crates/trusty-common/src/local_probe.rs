//! Liveness probe for a local OpenAI-compatible model server (#4490).
//!
//! Why: two call sites answer the same question — "is a local model server
//! actually reachable right now?" — and a wrong answer costs money and latency
//! silently instead of failing loudly. `chat::auto_detect_local_provider` has
//! probed since the `ChatProvider` era, while `inference::providers::local`
//! built an adapter for a fixed base URL unconditionally, so a consumer moving
//! from `ChatProvider` to `InferenceAdapter` (#4427) would have fallen through
//! to OpenRouter on every turn even with a local model running. Copying the
//! probe across would leave two independent implementations of one capability,
//! which CLAUDE.md's common-entry-point rule forbids, so the probe lives here
//! and both callers route through it — the timeout included, as one named
//! constant rather than a literal repeated per call site.
//!
//! What: [`LOCAL_PROBE_TIMEOUT`](crate::local_probe::LOCAL_PROBE_TIMEOUT) (the
//! shared budget, applied to both connect and whole-request),
//! [`models_url`](crate::local_probe::models_url) (the `{host}/v1/models`
//! derivation),
//! [`probe_models_endpoint`](crate::local_probe::probe_models_endpoint) (the GET
//! plus status check), and [`probe_local`](crate::local_probe::probe_local) (the
//! two composed). Failure is a typed
//! [`LocalProbeError`](crate::local_probe::LocalProbeError) that always names the
//! endpoint it dialled, so a caller can report which address was dead rather than
//! surfacing a bare transport timeout.
//!
//! Paths here are crate-absolute on purpose: this module carries docs both here
//! and on the `pub mod local_probe;` declaration in `lib.rs`, rustdoc merges the
//! two, and link resolution takes its scope from the FIRST fragment — the
//! `lib.rs` one — so a bare `LOCAL_PROBE_TIMEOUT` resolves against the crate root
//! and is not found (#6027).
//!
//! Scope: this deliberately does NOT route through
//! [`crate::http_client::loopback_client_builder`]. That entry point disables
//! proxies for LOOPBACK daemon targets, and a local model server is pointable at
//! a non-loopback host (`OLLAMA_HOST=http://192.168.1.50:11434`) where the
//! operator's proxy must still apply — the same carve-out `http_client`'s own
//! docs state for inference providers. [`crate::health_probe::probe_health`] is
//! the loopback-daemon counterpart; it answers `bool`, which cannot carry the
//! endpoint a caller has to report.
//!
//! Test: inline `tests` — `models_url_appends_v1_when_absent`,
//! `models_url_does_not_double_an_existing_v1_suffix`,
//! `probe_reports_unreachable_naming_the_endpoint`,
//! `probe_reports_non_success_status`, `probe_accepts_a_live_endpoint`,
//! `probe_timeout_is_one_second`.

use std::time::Duration;

/// The budget one liveness probe gets, for connect AND for the whole request.
///
/// Why: the probe runs on a startup path in front of a fallback decision, so it
/// must never be the thing that makes a command feel hung — a local server that
/// is not up has to be ruled out in about the time a human would wait. One
/// second is what `chat::auto_detect_local_provider` has used since it shipped;
/// naming it once here is what keeps the two callers from drifting apart.
/// What: one second, passed to both `connect_timeout` and `timeout`.
/// Test: `probe_timeout_is_one_second`.
pub const LOCAL_PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// The OpenAI-compatible path every local server implements for a cheap,
/// side-effect-free liveness check.
pub const LOCAL_MODELS_PATH: &str = "/v1/models";

/// Why the local model server could not be confirmed live.
///
/// Why: the caller's job after a failed probe is to tell an operator WHICH
/// address was dead — "connection refused" alone sends them looking at the wrong
/// host when `OLLAMA_HOST` points somewhere unexpected. Every variant therefore
/// carries the endpoint, and [`Self::endpoint`] reads it back without a match.
/// What: `ClientBuild` (the HTTP client could not be constructed at all),
/// `Unreachable` (no response inside [`LOCAL_PROBE_TIMEOUT`] — refused, timed
/// out, DNS, TLS), and `Status` (the server answered, but not 2xx). No variant
/// carries a credential: the probe sends no `Authorization` header.
/// Test: `probe_reports_unreachable_naming_the_endpoint`,
/// `probe_reports_non_success_status`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LocalProbeError {
    /// The `reqwest` client could not be built (never observed in practice).
    ClientBuild {
        /// The endpoint the probe would have dialled.
        endpoint: String,
        /// The stringified builder error.
        cause: String,
    },
    /// No response arrived within [`LOCAL_PROBE_TIMEOUT`].
    Unreachable {
        /// The endpoint that did not answer.
        endpoint: String,
        /// The stringified transport error.
        cause: String,
    },
    /// The server answered with a non-2xx status.
    Status {
        /// The endpoint that answered.
        endpoint: String,
        /// The HTTP status it returned.
        status: u16,
    },
}

impl LocalProbeError {
    /// The endpoint this probe dialled.
    ///
    /// Why: every caller reports it, and none of them should have to match on
    /// the variant to get at it.
    /// What: the `endpoint` field of whichever variant this is.
    /// Test: `probe_reports_unreachable_naming_the_endpoint`.
    pub fn endpoint(&self) -> &str {
        match self {
            Self::ClientBuild { endpoint, .. }
            | Self::Unreachable { endpoint, .. }
            | Self::Status { endpoint, .. } => endpoint,
        }
    }
}

impl std::fmt::Display for LocalProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ClientBuild { endpoint, cause } => write!(
                f,
                "could not build a probe client for local model server {endpoint}: {cause}"
            ),
            Self::Unreachable { endpoint, cause } => write!(
                f,
                "local model server not reachable at {endpoint} within {}s: {cause}",
                LOCAL_PROBE_TIMEOUT.as_secs()
            ),
            Self::Status { endpoint, status } => write!(
                f,
                "local model server at {endpoint} answered HTTP {status}, not a success status"
            ),
        }
    }
}

impl std::error::Error for LocalProbeError {}

/// Derive the `/v1/models` probe URL from a local server's base URL.
///
/// Why: the two callers spell their base URL differently and neither should have
/// to know the other's convention. `chat::LocalModelConfig::base_url` is a bare
/// host (`http://localhost:11434`); `inference::providers::local::LOCAL_BASE_URL`
/// already carries the `/v1` suffix the OpenAI dialect needs. One derivation that
/// accepts both is what lets a single probe serve both.
/// What: trims trailing slashes, drops a trailing `/v1` if present, and appends
/// [`LOCAL_MODELS_PATH`]. A bare host is therefore unchanged from what
/// `chat::auto_detect_local_provider` has always produced.
/// Test: `models_url_appends_v1_when_absent`,
/// `models_url_does_not_double_an_existing_v1_suffix`.
pub fn models_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let host = base.strip_suffix("/v1").unwrap_or(base);
    format!("{host}{LOCAL_MODELS_PATH}")
}

/// GET an already-derived models endpoint and report whether it is live.
///
/// Why: the single implementation of the probe itself, so the timeout, the
/// success criterion, and the error text cannot drift between `chat::` and
/// `inference::`.
/// What: builds a client bounded by [`LOCAL_PROBE_TIMEOUT`] on connect and on
/// the whole request, GETs `url`, and returns `Ok(())` for any 2xx. Anything
/// else — a build failure, a transport failure, a timeout, a non-2xx status — is
/// a [`LocalProbeError`] naming `url`. Sends no credential.
/// Test: `probe_reports_unreachable_naming_the_endpoint`,
/// `probe_reports_non_success_status`, `probe_accepts_a_live_endpoint`.
pub async fn probe_models_endpoint(url: &str) -> Result<(), LocalProbeError> {
    // #4490: proxies are deliberately left enabled — see the module's Scope note.
    let client = reqwest::Client::builder()
        .connect_timeout(LOCAL_PROBE_TIMEOUT)
        .timeout(LOCAL_PROBE_TIMEOUT)
        .build()
        .map_err(|e| LocalProbeError::ClientBuild {
            endpoint: url.to_string(),
            cause: e.to_string(),
        })?;

    match client.get(url).send().await {
        Ok(resp) if resp.status().is_success() => Ok(()),
        Ok(resp) => Err(LocalProbeError::Status {
            endpoint: url.to_string(),
            status: resp.status().as_u16(),
        }),
        Err(e) => Err(LocalProbeError::Unreachable {
            endpoint: url.to_string(),
            cause: e.to_string(),
        }),
    }
}

/// Probe a local model server given its base URL.
///
/// Why: the form both callers actually want — they hold a base URL, not a models
/// endpoint.
/// What: [`models_url`] followed by [`probe_models_endpoint`].
/// Test: `probe_accepts_a_live_endpoint`,
/// `probe_reports_unreachable_naming_the_endpoint`.
pub async fn probe_local(base_url: &str) -> Result<(), LocalProbeError> {
    probe_models_endpoint(&models_url(base_url)).await
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A loopback stub answering one canned status per connection, forever.
    ///
    /// Modelled on `http_client::tests::stub_server` — a real listener, so the
    /// probe under test drives the real transport.
    async fn stub_server(response: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback stub");
        let addr = listener.local_addr().expect("stub addr").to_string();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    use tokio::io::AsyncWriteExt;
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        addr
    }

    /// An address with nothing listening: bind port 0, read it, release it.
    fn dead_addr() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind to free a port");
        let addr = listener.local_addr().expect("dead addr").to_string();
        drop(listener);
        addr
    }

    /// Why: a bare host is what `chat::LocalModelConfig::base_url` holds, and
    /// the URL this produces for it must stay byte-identical to the one
    /// `chat::auto_detect_local_provider` has always built.
    /// Test: this test.
    #[test]
    fn models_url_appends_v1_when_absent() {
        assert_eq!(
            models_url("http://localhost:11434"),
            "http://localhost:11434/v1/models"
        );
        assert_eq!(
            models_url("http://localhost:11434/"),
            "http://localhost:11434/v1/models"
        );
    }

    /// Why: `inference::providers::local::LOCAL_BASE_URL` already ends in `/v1`,
    /// so a naive append would dial `/v1/v1/models` and report every live server
    /// as dead.
    /// Test: this test.
    #[test]
    fn models_url_does_not_double_an_existing_v1_suffix() {
        assert_eq!(
            models_url("http://localhost:11434/v1"),
            "http://localhost:11434/v1/models"
        );
        assert_eq!(
            models_url("http://localhost:11434/v1/"),
            "http://localhost:11434/v1/models"
        );
    }

    /// Why: the whole point of the typed error is that a caller can name the
    /// address that was dead; a bare "connection refused" sends an operator to
    /// the wrong host.
    /// Test: this test.
    #[tokio::test]
    async fn probe_reports_unreachable_naming_the_endpoint() {
        let addr = dead_addr();
        let base = format!("http://{addr}");
        let err = probe_local(&base).await.expect_err("closed port must fail");
        assert!(
            matches!(err, LocalProbeError::Unreachable { .. }),
            "expected Unreachable, got {err:?}"
        );
        assert_eq!(err.endpoint(), format!("{base}/v1/models"));
        assert!(err.to_string().contains(&addr), "{err}");
    }

    /// Why: a server that answers but does not serve `/v1/models` is not a
    /// usable local model server, and must be rejected rather than treated as
    /// live because a socket accepted the connection.
    /// Test: this test.
    #[tokio::test]
    async fn probe_reports_non_success_status() {
        let addr = stub_server("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n").await;
        let err = probe_local(&format!("http://{addr}"))
            .await
            .expect_err("404 must fail");
        assert_eq!(
            err,
            LocalProbeError::Status {
                endpoint: format!("http://{addr}/v1/models"),
                status: 404,
            }
        );
    }

    /// Why: the positive half — a reachable server must pass, or the probe would
    /// disable local inference entirely.
    /// Test: this test.
    #[tokio::test]
    async fn probe_accepts_a_live_endpoint() {
        let addr = stub_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await;
        probe_local(&format!("http://{addr}"))
            .await
            .expect("live endpoint must probe clean");
    }

    /// Why (#4490): the budget is a contract, not an implementation detail —
    /// both callers depend on a failed probe costing about a second, and this is
    /// the single definition they share.
    /// Test: this test.
    #[test]
    fn probe_timeout_is_one_second() {
        assert_eq!(LOCAL_PROBE_TIMEOUT, Duration::from_secs(1));
    }
}
