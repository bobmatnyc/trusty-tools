//! Opt-in deny-by-default persona-confirmation gate for delegated (sub-agent)
//! edit-tool dispatches under `tm hook --pm-guard` (issue #2231).
//!
//! Why: `pm_guard`'s Guard 4 (see `super::pm_guard::pm_guard`) unconditionally
//! exempts a native Task/Agent-tool sub-agent dispatch from every deny rule —
//! correct by default (issue #2014: the PM legitimately delegates edits to
//! rust-engineer and friends), but it means a managed session whose persona
//! deployment *positively, provably* failed would still let a delegated
//! Edit/Write through unchallenged. Some operators want a stricter posture:
//! refuse delegated edits too until the session's persona is confirmed. That
//! must be strictly OPT-IN — issue #2172 is the standing lesson here: a
//! fail-closed persona/spawn gate previously aborted real managed-session
//! launches because it could not positively confirm success, even for
//! perfectly healthy ad-hoc/`tm connect`/vanilla sessions that were never
//! managed in the first place. This module repeats none of that: the new
//! check is (a) OFF unless [`DENY_BY_DEFAULT_ENV`] is explicitly truthy, and
//! (b) even when on, it only ever denies on POSITIVE evidence of failure
//! (`state == "errored"`, the daemon's own definitive terminal-failure
//! record); every ambiguous or unmanaged case allows, exactly like the
//! pre-#2231 default.
//! What: [`deny_by_default_enabled`] reads the opt-in env var (default:
//! off — see [`is_truthy`] for the accepted spellings, shared with this
//! crate's other boolean env-var parsers). [`persona_status`] resolves a
//! [`PersonaStatus`] for the CURRENT pane by reading the `TM_MANAGED_SESSION_ID`
//! pane env var (exported by `runtime::claude_code::session_id_export_prefix`
//! into every managed session's shell — absent for ad-hoc/`tm connect`/vanilla
//! sessions) and, only when present, querying the daemon's
//! `GET /api/v1/sessions/managed/{id}` endpoint (via [`trusty_mpm::client::DaemonClient`],
//! the same typed call `guided.rs`'s `nested_session_guard` already uses) with a
//! tight connect+total timeout so an unreachable/slow daemon degrades to
//! `Ambiguous` (allow) rather than hanging the hook. [`classify_persona_status`]
//! is the pure decision core: no session id at all is [`PersonaStatus::NotManaged`];
//! a resolved `state == "errored"` is [`PersonaStatus::ExplicitlyFailed`]; any
//! other resolved state is [`PersonaStatus::Confirmed`]; a lookup that could not
//! be resolved (daemon unreachable, id unknown, malformed id) is
//! [`PersonaStatus::Ambiguous`]. [`PersonaStatus::should_deny`] is the ONLY
//! place that maps a status to a gating decision — `true` solely for
//! `ExplicitlyFailed`, so every other branch (including every error path)
//! resolves to allow.
//! Test: `is_truthy_*`, `classify_persona_status_*`,
//! `persona_status_for_session_not_managed_when_session_id_absent`,
//! `persona_status_for_session_ambiguous_when_daemon_unreachable`,
//! `persona_status_should_deny_only_for_explicitly_failed`.

use trusty_mpm::client::DaemonClient;

/// Opt-in env var (issue #2231): when truthy (see [`is_truthy`]), a native
/// sub-agent dispatch of an otherwise-guarded tool (Edit/Write/MultiEdit/
/// NotebookEdit, or a guarded Bash verb — i.e. anything
/// `super::pm_guard::evaluate_tool` would deny for the top-level PM) is
/// DENIED unless [`persona_status`] resolves to [`PersonaStatus::Confirmed`],
/// [`PersonaStatus::NotManaged`], or [`PersonaStatus::Ambiguous`]. Unset (the
/// default) leaves Guard 4's unconditional sub-agent exemption completely
/// unchanged.
pub(crate) const DENY_BY_DEFAULT_ENV: &str = "TRUSTY_MPM_PM_DENY_BY_DEFAULT";

/// The pane env var a managed session's launcher exports (see
/// `runtime::claude_code::session_id_export_prefix`), naming the managed
/// session this pane belongs to. Absent for ad-hoc/`tm connect`/vanilla
/// `claude` invocations — there was never a launcher to export it.
const MANAGED_SESSION_ID_ENV: &str = "TM_MANAGED_SESSION_ID";

/// Deny reason surfaced when the persona gate denies a delegated dispatch.
pub(crate) const PERSONA_DENY_REASON: &str = "PM session persona deployment explicitly failed \
     (TRUSTY_MPM_PM_DENY_BY_DEFAULT strict mode). Delegated edit blocked until the managed \
     session's persona is repaired (see `tm doctor`) or the session is restarted.";

/// Parse a truthy token, shared spelling with this crate's other boolean env
/// parsers (`core::auto_resume::parse_truthy`, `supervisor::config::env_bool`).
///
/// Why: one accepted-spellings list keeps every opt-in boolean flag in this
/// crate consistent instead of each guard inventing its own.
/// What: case-insensitive match against `1`/`true`/`yes`/`on`; `None` or any
/// other value is not truthy.
/// Test: `is_truthy_accepts_known_tokens`, `is_truthy_rejects_unknown_or_absent`.
pub(crate) fn is_truthy(raw: Option<&str>) -> bool {
    raw.is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// Whether the [`DENY_BY_DEFAULT_ENV`] opt-in is active for this process.
///
/// Why: split out so every call site shares one definition of "on".
/// What: `true` iff [`DENY_BY_DEFAULT_ENV`] is set to a truthy token.
/// Test: covered via `is_truthy_*` (this is a one-line env wrapper around it).
pub(crate) fn deny_by_default_enabled() -> bool {
    is_truthy(std::env::var(DENY_BY_DEFAULT_ENV).ok().as_deref())
}

/// Resolved persona-confirmation status for the current pane (issue #2231).
///
/// Why: the deny-by-default gate must distinguish "nothing to confirm" (not a
/// managed session), "confirmed good", "positively confirmed bad", and
/// "cannot tell" — collapsing the first three into "allow" and the fourth
/// bucket into "allow" too is the entire #2172-lesson safeguard; see
/// [`Self::should_deny`].
/// What: one variant per resolvable state; see [`classify_persona_status`] for
/// the pure mapping from raw inputs to this type.
/// Test: `classify_persona_status_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PersonaStatus {
    /// No `TM_MANAGED_SESSION_ID` at all — an ad-hoc/`tm connect`/vanilla
    /// session with no launcher, hence nothing to have failed.
    NotManaged,
    /// A managed session whose daemon-reported state is anything other than
    /// `errored` (e.g. `active`, `provisioning`, `stopped`).
    Confirmed,
    /// A managed session whose daemon-reported state is `errored` — a
    /// concrete, positive record of failure, not merely absent evidence.
    ExplicitlyFailed,
    /// A managed session id is present but its state could not be resolved
    /// (daemon unreachable/slow, id unknown to the daemon, malformed id).
    /// This is deliberately NOT treated the same as `ExplicitlyFailed` — the
    /// #2172 lesson is that absence of confirmation must never be treated as
    /// confirmation of failure.
    Ambiguous,
}

impl PersonaStatus {
    /// Maximally-permissive mapping to a gating decision (the #2172 lesson,
    /// applied here): only positive, unambiguous evidence of failure denies.
    ///
    /// Why: this is the ONE place a [`PersonaStatus`] becomes an ALLOW/DENY
    /// verdict, so the permissive default is enforced in exactly one spot.
    /// What: `true` only for [`Self::ExplicitlyFailed`]; every other variant
    /// (including `Ambiguous`) is `false` (allow).
    /// Test: `persona_status_should_deny_only_for_explicitly_failed`.
    pub(crate) fn should_deny(self) -> bool {
        matches!(self, Self::ExplicitlyFailed)
    }
}

/// Pure decision core: map (session-id presence, resolved daemon state) to a
/// [`PersonaStatus`].
///
/// Why: isolating the decision from the env-read and the HTTP call makes the
/// actual policy exhaustively unit-testable without a daemon or process env
/// mutation — the same "pure predicate, thin I/O wrapper" split already used
/// by `guided::nested_managed_match` / `nested_session_guard` in this crate.
/// What: no session id at all is [`PersonaStatus::NotManaged`]; a resolved
/// state string equal to `"errored"` (the exact `ManagedSessionState::Errored`
/// `Display` spelling — see `daemon::managed_routes::mod::` `SessionSummary`
/// construction, `state: record.state.to_string()`) is
/// [`PersonaStatus::ExplicitlyFailed`]; any other resolved state is
/// [`PersonaStatus::Confirmed`]; `None` (lookup failed/unresolved) with a
/// session id present is [`PersonaStatus::Ambiguous`].
/// Test: `classify_persona_status_not_managed_without_session_id`,
/// `classify_persona_status_confirmed_for_non_errored_state`,
/// `classify_persona_status_explicitly_failed_for_errored_state`,
/// `classify_persona_status_ambiguous_when_state_unresolved`.
fn classify_persona_status(session_id_present: bool, state: Option<&str>) -> PersonaStatus {
    if !session_id_present {
        return PersonaStatus::NotManaged;
    }
    match state {
        Some("errored") => PersonaStatus::ExplicitlyFailed,
        Some(_) => PersonaStatus::Confirmed,
        None => PersonaStatus::Ambiguous,
    }
}

/// Best-effort daemon lookup of a managed session's lifecycle-state word.
///
/// Why: the daemon's `sessions.json`-backed `SessionRecord::state` is the
/// only place a genuine `Errored` verdict is recorded; this hook process must
/// not read that file directly (it may not share the daemon's resolved
/// framework root) nor block on a slow/down daemon, so it reuses the same
/// typed HTTP call `guided.rs`'s `nested_session_guard` already makes,
/// with its own short-lived, tightly-timed client (mirrors
/// `pm_guard::audit_denied_tool`'s connect/total timeout budget).
/// What: `Some(state)` (e.g. `"active"`, `"errored"`) on a successful
/// `GET /api/v1/sessions/managed/{id}`; `None` on ANY failure — client-build
/// error, connect/timeout, non-2xx, or malformed body — so a down daemon
/// degrades to [`PersonaStatus::Ambiguous`], never a hang or a deny.
/// Test: exercised end to end (unreachable daemon → `None`) via
/// `persona_status_for_session_ambiguous_when_daemon_unreachable`.
async fn lookup_session_state(url: &str, session_id: &str) -> Option<String> {
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(500))
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()?;
    let daemon = DaemonClient::with_client(client, url);
    daemon
        .get_managed_session(session_id)
        .await
        .ok()
        .map(|s| s.state)
}

/// Resolve [`PersonaStatus`] given an explicit (test-injectable) session id.
///
/// Why: separating the session-id SOURCE from the resolution logic lets tests
/// drive every branch (present/absent, reachable/unreachable daemon) without
/// mutating process-global env state.
/// What: `None`/empty `raw_session_id` short-circuits to
/// [`classify_persona_status`]`(false, None)` (= `NotManaged`) with no network
/// call at all; otherwise awaits [`lookup_session_state`] and classifies the
/// result.
/// Test: `persona_status_for_session_not_managed_when_session_id_absent`,
/// `persona_status_for_session_ambiguous_when_daemon_unreachable`.
pub(crate) async fn persona_status_for_session(
    raw_session_id: Option<&str>,
    url: &str,
) -> PersonaStatus {
    let Some(session_id) = raw_session_id.map(str::trim).filter(|s| !s.is_empty()) else {
        return classify_persona_status(false, None);
    };
    let state = lookup_session_state(url, session_id).await;
    classify_persona_status(true, state.as_deref())
}

/// Resolve [`PersonaStatus`] for the CURRENT pane by reading
/// [`MANAGED_SESSION_ID_ENV`] from the process environment.
///
/// Why: this is the production entry point `pm_guard`'s Guard 4 calls; the
/// pane-env read is the ONLY place this module touches `std::env` for the
/// session id, keeping [`persona_status_for_session`] fully test-injectable.
/// What: thin wrapper delegating to [`persona_status_for_session`].
/// Test: covered by the `persona_status_for_session_*` tests (this adds only
/// the env read, which mirrors the well-established `TM_MANAGED_SESSION_ID`
/// read in `guided.rs::nested_session_guard`).
pub(crate) async fn persona_status(url: &str) -> PersonaStatus {
    let raw = std::env::var(MANAGED_SESSION_ID_ENV).ok();
    persona_status_for_session(raw.as_deref(), url).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_truthy_accepts_known_tokens() {
        for t in ["1", "true", "TRUE", "Yes", "on", " on "] {
            assert!(is_truthy(Some(t)), "{t:?} should be truthy");
        }
    }

    #[test]
    fn is_truthy_rejects_unknown_or_absent() {
        for f in [
            None,
            Some("0"),
            Some("false"),
            Some("no"),
            Some("off"),
            Some(""),
        ] {
            assert!(!is_truthy(f), "{f:?} should not be truthy");
        }
    }

    #[test]
    fn classify_persona_status_not_managed_without_session_id() {
        // Case: no launcher ever ran (ad-hoc / `tm connect` / vanilla claude).
        assert_eq!(
            classify_persona_status(false, None),
            PersonaStatus::NotManaged
        );
    }

    #[test]
    fn classify_persona_status_confirmed_for_non_errored_state() {
        // Case (ii): managed session, deployment succeeded.
        for state in ["active", "provisioning", "stopped"] {
            assert_eq!(
                classify_persona_status(true, Some(state)),
                PersonaStatus::Confirmed,
                "state {state:?} must classify as Confirmed"
            );
        }
    }

    #[test]
    fn classify_persona_status_explicitly_failed_for_errored_state() {
        // Case (iii): managed session with a positive, recorded failure.
        assert_eq!(
            classify_persona_status(true, Some("errored")),
            PersonaStatus::ExplicitlyFailed
        );
    }

    #[test]
    fn classify_persona_status_ambiguous_when_state_unresolved() {
        // Case (iv, managed-but-inconclusive branch): a session id is present
        // but its state could not be resolved — must NOT be treated as failure.
        assert_eq!(
            classify_persona_status(true, None),
            PersonaStatus::Ambiguous
        );
    }

    #[test]
    fn persona_status_should_deny_only_for_explicitly_failed() {
        assert!(PersonaStatus::ExplicitlyFailed.should_deny());
        for allowed in [
            PersonaStatus::NotManaged,
            PersonaStatus::Confirmed,
            PersonaStatus::Ambiguous,
        ] {
            assert!(
                !allowed.should_deny(),
                "{allowed:?} must never deny (the #2172 permissive default)"
            );
        }
    }

    #[tokio::test]
    async fn persona_status_for_session_not_managed_when_session_id_absent() {
        // Case (iv, unmanaged branch): no id at all — allowed, and no network
        // call is even attempted (a bogus unroutable URL would otherwise hang).
        let status = persona_status_for_session(None, "http://127.0.0.1:1").await;
        assert_eq!(status, PersonaStatus::NotManaged);
    }

    #[tokio::test]
    async fn persona_status_for_session_ambiguous_when_daemon_unreachable() {
        // Fail-open on internal error: an unroutable daemon URL must resolve
        // to Ambiguous (allow), never hang and never deny.
        let status = persona_status_for_session(
            Some("11111111-1111-1111-1111-111111111111"),
            "http://127.0.0.1:1",
        )
        .await;
        assert_eq!(status, PersonaStatus::Ambiguous);
    }

    /// Spawn a one-shot HTTP mock daemon that replies to the
    /// `GET /api/v1/sessions/managed/{id}` request with a canned
    /// [`trusty_mpm::client::ManagedSessionSummary`]-shaped JSON body (issue
    /// #2231, live-round-trip coverage for cases ii/iii).
    ///
    /// Why: the unreachable-daemon test above only covers the fail-open
    /// branch; `persona_status_for_session`'s daemon-REACHABLE branch (state
    /// `"active"` vs `"errored"`) needs a real HTTP round trip through
    /// [`trusty_mpm::client::DaemonClient::get_managed_session`] to be
    /// genuinely exercised, not merely asserted via the pure
    /// `classify_persona_status` mapping. A raw one-shot [`TcpListener`] (no
    /// axum router) is the lightest dependency-free mock, mirroring the same
    /// technique `core::sm::providers::openrouter_tests::spawn_mock` uses in
    /// the library crate.
    /// What: binds an ephemeral port, accepts ONE connection, drains the
    /// request (a bare GET has no body, so one read suffices), writes a fixed
    /// 200 JSON response carrying `state`, and shuts down. Returns the bound
    /// `http://127.0.0.1:PORT` base URL.
    /// Test: used by `persona_status_for_session_confirmed_for_active_state`,
    /// `persona_status_for_session_explicitly_failed_for_errored_state`.
    async fn spawn_mock_daemon(state: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let body = format!(
            r#"{{"id":"11111111-1111-1111-1111-111111111111","name":"tmpm-test","state":"{state}"}}"#
        );
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn persona_status_for_session_confirmed_for_active_state() {
        // Case (ii): managed session, deployment succeeded — a live daemon
        // reporting `state: "active"` must resolve to Confirmed (allow).
        let url = spawn_mock_daemon("active").await;
        let status =
            persona_status_for_session(Some("11111111-1111-1111-1111-111111111111"), &url).await;
        assert_eq!(status, PersonaStatus::Confirmed);
    }

    #[tokio::test]
    async fn persona_status_for_session_explicitly_failed_for_errored_state() {
        // Case (iii): managed session with a positive, recorded failure — a
        // live daemon reporting `state: "errored"` must resolve to
        // ExplicitlyFailed (deny).
        let url = spawn_mock_daemon("errored").await;
        let status =
            persona_status_for_session(Some("11111111-1111-1111-1111-111111111111"), &url).await;
        assert_eq!(status, PersonaStatus::ExplicitlyFailed);
    }
}
