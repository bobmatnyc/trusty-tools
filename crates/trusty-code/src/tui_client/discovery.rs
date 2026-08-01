//! Daemon discovery for [`crate::tui_client::CodeEngine`] (issue #3415,
//! DOC-50 §3.4).
//!
//! Why: `tcode tui` needs to find an already-running `tcode serve --http`
//! daemon without the operator hand-copying a port. DOC-50 §3.4 specifies
//! the lookup priority — explicit override, then a discovery file, then a
//! liveness check — but its prose sketched a NEW `~/.trusty-code/daemon.json`
//! shape; `crate::serve::discovery` documents why this crate follows the
//! ALREADY-ESTABLISHED sibling convention (`trusty-memory`/`trusty-search`'s
//! plain-text `http_addr` file) instead. This module is the READ side of
//! that convention; `crate::serve::discovery`'s `write_http_addr_file` is
//! the write side, called from `crate::serve::http::run_http`.
//! What: [`lookup_daemon`] tries, in order, [`DAEMON_URL_ENV`] (an
//! explicit override — trusted without a liveness check of its own source,
//! but still ping-verified before use, same as the file-sourced candidate)
//! then the `http_addr` discovery file; whichever candidate is found is
//! verified alive via `GET {url}/health` (the SAME route
//! `crate::serve::methods::health_payload` answers, already the
//! ecosystem-standard liveness probe per `crate::serve::http`'s docs). It
//! reports the outcome as a three-way [`Lookup`] — live, found-but-dead
//! (naming its [`Source`]), or no candidate at all — because a CALLER has to
//! branch on WHICH source failed: `tcode tui` auto-spawns a daemon when the
//! discovery file is stale or absent, but must NOT when
//! [`DAEMON_URL_ENV`] explicitly named an address, since spawning
//! something at a DIFFERENT address would quietly ignore that instruction
//! (#4512). [`discover_daemon_url`] is the attach-only wrapper over it,
//! collapsing the same three outcomes into a [`DiscoveryError`] with an
//! actionable message — never a silent `None`/default.
//! Test: `discovery_tests::*` for the pure candidate-selection logic (env
//! var precedence, file fallback, "neither present" case) and the
//! [`Lookup`]-to-[`DiscoveryError`] collapse; the liveness-ping
//! branch is covered end-to-end in `tests/tui_client_engine.rs` against a
//! mock daemon (a unit test would need a real bound socket to ping, which
//! belongs in that hermetic integration suite, not here).

use std::time::Duration;

/// Environment variable that, when set to a non-empty value, names the
/// daemon URL directly — highest-priority discovery source (DOC-50 §3.4
/// point 1).
pub const DAEMON_URL_ENV: &str = "TCODE_DAEMON_URL";

/// How long the liveness ping (`GET {url}/health`) waits before treating the
/// candidate as dead. Short and fixed — this call happens once, at REPL
/// startup, and a slow/hung `/health` response is itself a strong "don't use
/// this daemon" signal.
const PING_TIMEOUT: Duration = Duration::from_secs(2);

/// Which discovery source produced a candidate URL — carried into
/// [`Lookup::Dead`] (and from there into [`DiscoveryError::NotAlive`]) so the
/// error message tells the operator WHERE the stale/wrong pointer came from,
/// and so `tcode tui`'s auto-spawn can tell an EXPLICIT instruction it must
/// obey ([`Source::Env`]) from a stale file it may replace ([`Source::File`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// [`DAEMON_URL_ENV`] named the URL — an explicit operator instruction.
    Env,
    /// The `http_addr` discovery file named the URL — a best-effort pointer
    /// a previous daemon left behind.
    File,
}

impl Source {
    /// Human-readable name for error messages ("from {found_via}").
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Env => "TCODE_DAEMON_URL",
            Source::File => "discovery file",
        }
    }
}

/// The three distinguishable outcomes of one daemon lookup.
///
/// Why: [`DiscoveryError`] collapses "found but dead" and "nothing found"
/// into two error variants that both mean "you can't attach", which is all
/// an attach-only caller needs. Auto-spawn needs MORE: it must refuse to
/// spawn when [`Source::Env`] named a dead address (spawning would land at a
/// different address and ignore the operator's explicit instruction), while
/// treating a dead or absent discovery file as a green light (#4512).
/// Test: `discovery_tests::lookup_collapses_into_discovery_errors`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// A candidate answered `GET {url}/health` — attach to it.
    Live(String),
    /// A candidate URL was named by `source` but is not responding.
    Dead { url: String, source: Source },
    /// Neither source named a candidate URL.
    Absent,
}

/// See module docs.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DiscoveryError {
    /// Neither `TCODE_DAEMON_URL` nor the discovery file named a candidate
    /// daemon URL.
    #[error(
        "no tcode daemon found — start one with `tcode serve --http`, or point \
         `tcode tui` at a running instance with `{DAEMON_URL_ENV}=http://host:port`"
    )]
    NotFound,

    /// A candidate URL was found (from `source`) but did not answer
    /// `GET {url}/health` with a success status within [`PING_TIMEOUT`].
    #[error(
        "found a tcode daemon reference at {url} (from {found_via}) but it is not responding \
         — is it still running? Start one with `tcode serve --http`, or update {DAEMON_URL_ENV} \
         to point at a live daemon"
    )]
    NotAlive {
        url: String,
        found_via: &'static str,
    },
}

/// Resolve a candidate daemon URL and report whether it is live, per this
/// module's docs.
///
/// Why/What: see module docs — this is the source-aware primitive
/// [`discover_daemon_url`] and `tcode tui`'s auto-spawn (#4512) are both
/// built on. `client` is caller-supplied (rather than built internally) so
/// `CodeEngine` can reuse the SAME pooled `reqwest::Client` for both this
/// ping and every later `POST /rpc`/SSE call — one connection pool for the
/// engine's whole lifetime, not one throwaway client per call.
/// Test: `discovery_tests::env_var_wins_over_file`,
/// `discovery_tests::env_var_trailing_slash_is_stripped`,
/// `discovery_tests::blank_env_var_is_ignored` cover candidate selection;
/// the liveness branch is covered by `tests/tui_client_engine.rs` and (for
/// the spawn decision it feeds) `cli::daemon_autospawn`'s tests.
pub async fn lookup_daemon(client: &reqwest::Client) -> Lookup {
    let Some((url, source)) = candidate_url() else {
        return Lookup::Absent;
    };
    if ping_alive(client, &url).await {
        Lookup::Live(url)
    } else {
        Lookup::Dead { url, source }
    }
}

/// Resolve and verify a live daemon URL, failing when none can be attached
/// to — the attach-only view of [`lookup_daemon`].
///
/// Why: `CodeEngine::discover` (and every non-TUI caller) only ever wants
/// "give me a live daemon or an actionable error"; the three-way [`Lookup`]
/// distinction matters solely to the auto-spawn path.
/// Test: `discovery_tests::lookup_collapses_into_discovery_errors`;
/// end-to-end in `tests/tui_client_engine.rs`.
pub async fn discover_daemon_url(client: &reqwest::Client) -> Result<String, DiscoveryError> {
    collapse(lookup_daemon(client).await)
}

/// The pure [`Lookup`] -> [`DiscoveryError`] mapping, split out of
/// [`discover_daemon_url`] so it is unit testable without a bound socket.
fn collapse(lookup: Lookup) -> Result<String, DiscoveryError> {
    match lookup {
        Lookup::Live(url) => Ok(url),
        Lookup::Dead { url, source } => Err(DiscoveryError::NotAlive {
            url,
            found_via: source.as_str(),
        }),
        Lookup::Absent => Err(DiscoveryError::NotFound),
    }
}

/// Pick a candidate URL from the env var or the discovery file, per the
/// documented priority. Pure (no network I/O) so it's directly unit
/// testable.
fn candidate_url() -> Option<(String, Source)> {
    if let Ok(raw) = std::env::var(DAEMON_URL_ENV) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Some((trimmed.trim_end_matches('/').to_string(), Source::Env));
        }
    }
    let path = crate::serve::discovery::http_addr_path()?;
    let addr = crate::serve::discovery::read_http_addr_file(&path)?;
    Some((format!("http://{addr}"), Source::File))
}

/// `GET {base_url}/health`, bounded by [`PING_TIMEOUT`] — `true` iff it
/// returns a success (2xx) status.
async fn ping_alive(client: &reqwest::Client, base_url: &str) -> bool {
    let url = format!("{base_url}/health");
    match client.get(&url).timeout(PING_TIMEOUT).send().await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod discovery_tests {
    use super::*;

    /// Serializes every test in this module that mutates `DAEMON_URL_ENV` —
    /// mirrors `crate::task::mock_llm::MOCK_LLM_ENV_LOCK`'s established
    /// pattern for env-mutating tests in this crate (a plain `tokio::sync::Mutex`
    /// guard rather than pulling in `serial_test` for one file).
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Both a `TCODE_DAEMON_URL` and a discovery file present: the env var
    /// must win (DOC-50 §3.4 point 1's explicit priority).
    #[tokio::test]
    async fn env_var_wins_over_file() {
        let _guard = ENV_LOCK.lock().await;
        // SAFETY: test-only env mutation; serialized by `ENV_LOCK`.
        unsafe {
            std::env::set_var(DAEMON_URL_ENV, "http://127.0.0.1:9999");
        }
        let (url, source) = candidate_url().expect("candidate");
        unsafe {
            std::env::remove_var(DAEMON_URL_ENV);
        }
        assert_eq!(url, "http://127.0.0.1:9999");
        assert_eq!(source, Source::Env);
    }

    /// A trailing slash on the env var is stripped, so URL-joining callers
    /// (`{base}/rpc`, `{base}/health`) never produce a doubled `//`.
    #[tokio::test]
    async fn env_var_trailing_slash_is_stripped() {
        let _guard = ENV_LOCK.lock().await;
        // SAFETY: test-only env mutation; serialized by `ENV_LOCK`.
        unsafe {
            std::env::set_var(DAEMON_URL_ENV, "http://127.0.0.1:9999/");
        }
        let (url, _source) = candidate_url().expect("candidate");
        unsafe {
            std::env::remove_var(DAEMON_URL_ENV);
        }
        assert_eq!(url, "http://127.0.0.1:9999");
    }

    /// An empty/whitespace `TCODE_DAEMON_URL` must be treated as unset, not
    /// as a (broken) candidate.
    #[tokio::test]
    async fn blank_env_var_is_ignored() {
        let _guard = ENV_LOCK.lock().await;
        // SAFETY: test-only env mutation; serialized by `ENV_LOCK`.
        unsafe {
            std::env::set_var(DAEMON_URL_ENV, "   ");
        }
        // With the env var blank, resolution falls through to the discovery
        // file — assert only that the env var itself did not win.
        let result = candidate_url();
        unsafe {
            std::env::remove_var(DAEMON_URL_ENV);
        }
        if let Some((_, source)) = result {
            assert_ne!(source, Source::Env);
        }
    }

    /// `DiscoveryError::NotFound`'s message must name the actionable fix
    /// (start a daemon, or set the env var) — pinned so a future edit can't
    /// silently regress it into an unhelpful "not found".
    #[test]
    fn not_found_message_is_actionable() {
        let msg = DiscoveryError::NotFound.to_string();
        assert!(msg.contains("tcode serve --http"));
        assert!(msg.contains(DAEMON_URL_ENV));
    }

    /// Every `Lookup` outcome must collapse to the matching attach-only
    /// result — pinned because `tcode tui`'s auto-spawn now branches on the
    /// SAME three outcomes (#4512), and the two views must stay agreed on
    /// what each one means.
    #[test]
    fn lookup_collapses_into_discovery_errors() {
        assert_eq!(
            collapse(Lookup::Live("http://127.0.0.1:7882".to_string())),
            Ok("http://127.0.0.1:7882".to_string())
        );
        assert_eq!(
            collapse(Lookup::Dead {
                url: "http://127.0.0.1:7882".to_string(),
                source: Source::Env,
            }),
            Err(DiscoveryError::NotAlive {
                url: "http://127.0.0.1:7882".to_string(),
                found_via: "TCODE_DAEMON_URL",
            })
        );
        assert_eq!(collapse(Lookup::Absent), Err(DiscoveryError::NotFound));
    }

    /// `DiscoveryError::NotAlive`'s message must name both the dead URL and
    /// which source produced it.
    #[test]
    fn not_alive_message_names_url_and_source() {
        let err = DiscoveryError::NotAlive {
            url: "http://127.0.0.1:7882".to_string(),
            found_via: Source::File.as_str(),
        };
        let msg = err.to_string();
        assert!(msg.contains("http://127.0.0.1:7882"));
        assert!(msg.contains("discovery file"));
    }
}
