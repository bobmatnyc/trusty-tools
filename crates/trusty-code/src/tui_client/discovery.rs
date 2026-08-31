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
//! (#4512). A live daemon additionally carries the [`ReportedBinding`] it
//! published, because "a daemon is answering" is NOT the same question as
//! "that daemon serves the project I mean to work in" — see
//! [`ReportedBinding`]. [`discover_daemon_url`] is the attach-only wrapper
//! over it, collapsing the same three outcomes into a [`DiscoveryError`]
//! with an actionable message — never a silent `None`/default.
//! Test: `discovery_tests::*` for the pure candidate-selection logic (env
//! var precedence, file fallback, "neither present" case), the
//! [`Lookup`]-to-[`DiscoveryError`] collapse, and [`ReportedBinding`]
//! parsing; the liveness-ping branch is covered end-to-end in
//! `tests/tui_client_engine.rs` against a mock daemon (a unit test would need
//! a real bound socket to ping, which belongs in that hermetic integration
//! suite, not here).

use std::path::PathBuf;
use std::time::Duration;

use crate::binding::ProjectBinding;

/// Environment variable that, when set to a non-empty value, names the
/// daemon URL directly — highest-priority discovery source (DOC-50 §3.4
/// point 1).
pub const DAEMON_URL_ENV: &str = "TCODE_DAEMON_URL";

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
/// else. `TCODE_DAEMON_URL` can legitimately name a non-loopback address (an
/// operator forwarding a port, a remote daemon over a tunnel), and attaching
/// the local machine's credential to a request leaving loopback would hand it
/// to whatever answers. This crate resolves it in one place so the three
/// request sites — `RpcHttpClient`, the SSE readers, and `probe_health` —
/// cannot each grow their own answer.
/// What: a thin naming of `trusty_common::daemon_token::credential_for`, which
/// owns the loopback gate, the override precedence, and the file read.
///
/// The gate lives THERE and not here for a reason worth stating: the first
/// version of this function called `server::origin_is_loopback`, an
/// `Origin`-HEADER parser, which reads
/// `http://127.0.0.1:7882@attacker.example` as loopback and shipped the token
/// off-machine. `trusty-code-gui` had the identical bug in its own copy. One
/// implementation, parsing the way the client that dials the URL parses.
/// Test: `discovery_tests::credential_is_withheld_from_a_non_loopback_url`,
/// `discovery_tests::credential_env_override_wins_for_a_loopback_url`.
pub fn daemon_credential_for(base_url: &str) -> Option<String> {
    trusty_common::daemon_token::credential_for(
        crate::serve::http::TOKEN_APP_NAME,
        base_url,
        DAEMON_TOKEN_ENV,
    )
}

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

/// The project binding a live daemon published on `GET /health` (#4512).
///
/// Why: a `tcode serve --http` daemon binds exactly ONE `ProjectBinding` at
/// `crate::serve::build_router` time and keeps it for its whole life. Until
/// #4512 that binding was invisible to clients, which was harmless while
/// `tcode tui` refused to run without a hand-started daemon — the operator
/// who started it knew what it was bound to. Auto-attach removes that
/// guarantee: a TUI launched in project B finds project A's daemon on the
/// well-known port and would drive it, so every session, index, and file
/// operation would silently land in the wrong project. Making the binding
/// part of the lookup result is what lets a caller refuse.
/// What: the daemon's bound project root, or an explicit projectless state,
/// or [`Unreported`](ReportedBinding::Unreported) when `/health` answered but
/// carried no usable `binding` field — a daemon older than #4512, or one
/// whose payload could not be parsed. Those two are deliberately the same
/// variant: both mean "this daemon's project CANNOT be verified", and a
/// caller must treat an unverifiable daemon the same way regardless of why.
/// Test: `discovery_tests::reported_binding_parses_every_health_shape`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportedBinding {
    /// The daemon reported it serves no project.
    Projectless,
    /// The daemon reported this canonical project root.
    Bound(PathBuf),
    /// `/health` answered, but with no parseable `binding` field.
    Unreported,
}

impl ReportedBinding {
    /// Read a [`ReportedBinding`] out of a `GET /health` body.
    ///
    /// Why/What: reuses `ProjectBinding`'s own `Deserialize` (the
    /// `{state, root}` wire shape defined once in `crate::binding`) rather
    /// than re-spelling that shape here, so the reader can never drift from
    /// the writer. A missing or malformed field is
    /// [`Unreported`](ReportedBinding::Unreported), never a guess.
    /// Test: `discovery_tests::reported_binding_parses_every_health_shape`.
    pub fn from_health(health: &serde_json::Value) -> Self {
        let Some(raw) = health.get("binding") else {
            return Self::Unreported;
        };
        match serde_json::from_value::<ProjectBinding>(raw.clone()) {
            Ok(binding) => match binding.root() {
                Some(root) => Self::Bound(root.to_path_buf()),
                None => Self::Projectless,
            },
            Err(_) => Self::Unreported,
        }
    }

    /// How to name this binding in an operator-facing message.
    ///
    /// Why: a mismatch error has to print BOTH sides, and "no project" has to
    /// read as a deliberate state rather than as missing information.
    /// Test: `discovery_tests::reported_binding_describes_each_state`.
    pub fn describe(&self) -> String {
        match self {
            Self::Projectless => "<projectless>".to_string(),
            Self::Bound(root) => root.display().to_string(),
            Self::Unreported => "<unreported — daemon predates #4512>".to_string(),
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
/// treating a dead or absent discovery file as a green light (#4512). It
/// also needs the live daemon's [`ReportedBinding`], since attaching to a
/// daemon bound to a DIFFERENT project is worse than not attaching at all.
/// Test: `discovery_tests::lookup_collapses_into_discovery_errors`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// A candidate answered `GET {url}/health` — attach to it, if `binding`
    /// is the project the caller wants.
    Live {
        url: String,
        binding: ReportedBinding,
    },
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
    match probe_health(client, &url).await {
        Some(health) => Lookup::Live {
            url,
            binding: ReportedBinding::from_health(&health),
        },
        None => Lookup::Dead { url, source },
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
        Lookup::Live { url, .. } => Ok(url),
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

/// `GET {base_url}/health`, bounded by [`PING_TIMEOUT`] — `Some(body)` iff
/// it returns a success (2xx) status with a JSON body.
///
/// #4512: this used to answer a bare `bool`. The BODY is now needed too, for
/// the `binding` field the caller checks before attaching; a 2xx whose body
/// will not parse as JSON still counts as alive (the daemon is answering),
/// and degrades to `Value::Null`, from which
/// [`ReportedBinding::from_health`] correctly reports `Unreported`.
///
/// #5439/#6472: `/health` stays reachable without a credential, but it answers
/// an anonymous caller with liveness ALONE. This probe therefore sends the
/// credential when it has one — without it the body carries no `binding`, and
/// [`ReportedBinding::from_health`] correctly reports `Unreported`, which the
/// caller must then treat as an unverifiable daemon.
async fn probe_health(client: &reqwest::Client, base_url: &str) -> Option<serde_json::Value> {
    let url = format!("{base_url}/health");
    let mut req = client.get(&url).timeout(PING_TIMEOUT);
    if let Some(token) = daemon_credential_for(base_url) {
        req = req.bearer_auth(token);
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    Some(resp.json().await.unwrap_or(serde_json::Value::Null))
}

#[cfg(test)]
mod discovery_tests {
    use super::*;

    /// Serializes every test in this module that mutates `DAEMON_URL_ENV` —
    /// mirrors `crate::task::mock_llm::MOCK_LLM_ENV_LOCK`'s established
    /// pattern for env-mutating tests in this crate (a plain `tokio::sync::Mutex`
    /// guard rather than pulling in `serial_test` for one file).
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// #5439's credential-exfiltration guard: the local token authenticates a
    /// caller to the LOCAL daemon and must never leave loopback, however the
    /// operator points `TCODE_DAEMON_URL`.
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
            // and every request goes there. Gating on the header parser shipped
            // a credential-exfiltration path that the plain-hostname rows above
            // could never catch.
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
            collapse(Lookup::Live {
                url: "http://127.0.0.1:7882".to_string(),
                binding: ReportedBinding::Projectless,
            }),
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

    /// Every shape a real `GET /health` body can take must map to the right
    /// `ReportedBinding` — including the two "cannot verify" shapes, which
    /// must NOT be mistaken for a projectless daemon (#4512).
    #[test]
    fn reported_binding_parses_every_health_shape() {
        use serde_json::json;

        let bound = json!({
            "server": "tcode",
            "binding": {"state": crate::binding::STATE_GIT_REPO, "root": "/tmp/proj"},
        });
        assert_eq!(
            ReportedBinding::from_health(&bound),
            ReportedBinding::Bound(PathBuf::from("/tmp/proj"))
        );

        let projectless = json!({
            "binding": {"state": crate::binding::STATE_PROJECTLESS, "root": null},
        });
        assert_eq!(
            ReportedBinding::from_health(&projectless),
            ReportedBinding::Projectless
        );

        // A pre-#4512 daemon: `/health` answers, but with no binding at all.
        let old = json!({"server": "tcode", "version": "0.2.0", "status": "ok"});
        assert_eq!(
            ReportedBinding::from_health(&old),
            ReportedBinding::Unreported,
            "a daemon that reports nothing must never read as projectless"
        );

        // A malformed payload is equally unverifiable, never a guess.
        let malformed = json!({"binding": {"state": "who knows"}});
        assert_eq!(
            ReportedBinding::from_health(&malformed),
            ReportedBinding::Unreported
        );
        assert_eq!(
            ReportedBinding::from_health(&serde_json::Value::Null),
            ReportedBinding::Unreported
        );
    }

    /// Each state must render an operator-readable name — the mismatch error
    /// prints both sides, so "no project" must not come out blank.
    #[test]
    fn reported_binding_describes_each_state() {
        assert_eq!(ReportedBinding::Projectless.describe(), "<projectless>");
        assert_eq!(
            ReportedBinding::Bound(PathBuf::from("/tmp/proj")).describe(),
            "/tmp/proj"
        );
        assert!(
            ReportedBinding::Unreported.describe().contains("4512"),
            "an unverifiable daemon must say why it cannot be checked"
        );
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
