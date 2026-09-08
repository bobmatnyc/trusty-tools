//! The client-side credential for the daemon's HTTP listener (#5439, #6637).
//!
//! Why this is its own module now: it used to live in
//! `crate::tui_client::discovery`, alongside the `TCODE_DAEMON_URL` lookup the
//! TUI used to find a daemon. #6637 moved the TUI onto the daemon's Unix
//! socket, where the peer-uid check replaces the token outright, and deleted
//! that module. [`crate::session::connector::TcodeConnector`] still speaks
//! HTTP, so the credential half outlives the discovery half and moves here
//! rather than being deleted with it.
//!
//! **This retires with the TCP listener in PR 2** — `trusty-code-gui` serves
//! its own webview then, and no client in this crate dials HTTP.
//!
//! What: [`DAEMON_TOKEN_ENV`](crate::http_credential::DAEMON_TOKEN_ENV) and
//! [`daemon_credential_for`](crate::http_credential::daemon_credential_for),
//! unchanged.
//!
//! Test: `http_credential_tests`.

/// Environment variable naming the daemon credential directly, ahead of the
/// token file (#5439).
///
/// Why: a client may run where it cannot read the daemon's data directory — a
/// container, a different account, an operator driving a remote daemon over a
/// tunnel. This is a CLIENT-side override only: `tcode serve --http` never
/// reads it, because a server that took its credential from the environment
/// would accept whatever a caller could arrange to export.
pub const DAEMON_TOKEN_ENV: &str = "TCODE_DAEMON_TOKEN";

/// The credential to send to `base_url`, or `None` when there is none to send
/// or none that may be sent.
///
/// Why: the token authenticates a caller to the LOCAL daemon, and nothing
/// else. A base URL can legitimately name a non-loopback address (an operator
/// forwarding a port, a remote daemon over a tunnel), and attaching the local
/// machine's credential to a request leaving loopback would hand it to
/// whatever answers. This crate resolves it in one place so no request site
/// grows its own answer.
/// What: a thin naming of `trusty_common::daemon_token::credential_for`, which
/// owns the loopback gate, the override precedence, and the file read.
///
/// The gate lives THERE and not here for a reason worth stating: the first
/// version of this function called `server::origin_is_loopback`, an
/// `Origin`-HEADER parser, which reads
/// `http://127.0.0.1:7882@attacker.example` as loopback and shipped the token
/// off-machine. `trusty-code-gui` had the identical bug in its own copy. One
/// implementation, parsing the way the client that dials the URL parses.
/// Test: `http_credential_tests::credential_is_withheld_from_a_non_loopback_url`,
/// `http_credential_tests::credential_env_override_wins_for_a_loopback_url`.
pub fn daemon_credential_for(base_url: &str) -> Option<String> {
    trusty_common::daemon_token::credential_for(
        crate::serve::http::TOKEN_APP_NAME,
        base_url,
        DAEMON_TOKEN_ENV,
    )
}

#[cfg(test)]
mod http_credential_tests {
    use super::*;

    /// Serializes every test in this module that mutates [`DAEMON_TOKEN_ENV`]
    /// — mirrors `crate::task::mock_llm::MOCK_LLM_ENV_LOCK`'s established
    /// pattern for env-mutating tests in this crate.
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// #5439's credential-exfiltration guard: the local token authenticates a
    /// caller to the LOCAL daemon and must never leave loopback, however the
    /// operator points a client.
    ///
    /// This is the arm that fails open if the gate is dropped — with a token
    /// available in the environment, a non-loopback base URL must still
    /// resolve to `None`.
    #[tokio::test]
    async fn credential_is_withheld_from_a_non_loopback_url() {
        let _guard = ENV_LOCK.lock().await;
        // SAFETY: test-only env mutation; serialized by `ENV_LOCK`.
        unsafe {
            std::env::set_var(DAEMON_TOKEN_ENV, "a".repeat(64));
        }
        let remote = [
            "http://example.test:7882",
            "https://10.0.0.5:7882",
            "http://192.168.1.4:7882",
            // The userinfo family. An `Origin`-header parser splits the
            // authority at the FIRST `:`, so it reads the host of these as
            // `127.0.0.1` and calls them loopback; WHATWG URL parsing splits
            // userinfo at the LAST `@`, so the real host is `attacker.example`
            // and every request goes there.
            "http://127.0.0.1:7882@attacker.example",
            "http://127.0.0.1:7882@attacker.example/rpc",
            "http://localhost@attacker.example",
            "http://user:pass@attacker.example",
        ]
        .map(daemon_credential_for);
        let local = daemon_credential_for("http://127.0.0.1:7882");
        unsafe {
            std::env::remove_var(DAEMON_TOKEN_ENV);
        }
        for (url, resolved) in [
            "example.test",
            "10.0.0.5",
            "192.168.1.4",
            "127.0.0.1:7882@attacker.example",
            "127.0.0.1:7882@attacker.example/rpc",
            "localhost@attacker.example",
            "user:pass@attacker.example",
        ]
        .iter()
        .zip(remote.iter())
        {
            assert_eq!(resolved.as_deref(), None, "{url} must get no credential");
        }
        assert_eq!(
            local.as_deref(),
            Some("a".repeat(64).as_str()),
            "loopback must get the credential"
        );
    }

    /// The env override must beat the token file, so a client that cannot read
    /// the daemon's data directory can still be pointed at a credential.
    #[tokio::test]
    async fn credential_env_override_wins_for_a_loopback_url() {
        let _guard = ENV_LOCK.lock().await;
        // SAFETY: test-only env mutation; serialized by `ENV_LOCK`.
        unsafe {
            std::env::set_var(DAEMON_TOKEN_ENV, "b".repeat(64));
        }
        let resolved = daemon_credential_for("http://localhost:7882");
        unsafe {
            std::env::remove_var(DAEMON_TOKEN_ENV);
        }
        assert_eq!(resolved.as_deref(), Some("b".repeat(64).as_str()));
    }

    /// A blank override must be ignored rather than becoming an empty
    /// credential — an empty bearer is a malformed header, not "no header".
    #[tokio::test]
    async fn blank_credential_override_falls_through() {
        let _guard = ENV_LOCK.lock().await;
        // SAFETY: test-only env mutation; serialized by `ENV_LOCK`.
        unsafe {
            std::env::set_var(DAEMON_TOKEN_ENV, "   ");
        }
        let resolved = daemon_credential_for("http://127.0.0.1:7882");
        unsafe {
            std::env::remove_var(DAEMON_TOKEN_ENV);
        }
        assert_ne!(
            resolved.as_deref(),
            Some(""),
            "a blank override must not become an empty credential"
        );
    }
}
