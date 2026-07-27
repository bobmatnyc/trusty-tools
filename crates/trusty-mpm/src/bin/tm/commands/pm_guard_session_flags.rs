//! Daemon-resolved `pm_guard` kill-switch flags for `pm_guard`'s Guards 2/3
//! (issue #3981 Part 2).
//!
//! Why: Guards 2/3 (`TRUSTY_MPM_DISABLE_HOOKS`/`TRUSTY_MPM_PM_UNRESTRICTED`)
//! used to read LIVE process env on every `PreToolUse` call. Claude Code
//! re-spawns the hook subprocess fresh each time and re-injects
//! `.claude/settings.json`'s `env` block into it, so the PM could self-exempt
//! from the entire guard by writing that file once — no restart, no
//! observable session disruption. This module replaces that with a daemon
//! round trip resolving the flags from the session's OWN record, captured
//! once at `tm sessions new`/`start`/`resume` time from the operator's
//! launching shell (`session_manager::guard_flags`,
//! `core::pm_guard_launch_env`) — `pm_guard` never writes these, only reads.
//! What: [`guard_flags_for_session`] mirrors
//! [`super::pm_guard_deny_by_default::persona_status_for_session`]'s exact
//! shape — same daemon endpoint family, same `EnvSessionIdentity` pane cross-
//! check (issue #3600: a sibling pane inheriting the same
//! `TM_MANAGED_SESSION_ID` must never be trusted). The ONE deliberate
//! difference from that precedent is the fail direction: `persona_status`
//! fails OPEN (permissive) because it is an ordinary ALLOW/DENY policy
//! check; resolving a KILL-SWITCH state must fail TOWARD THE GUARD STAYING
//! ACTIVE — a down daemon, an unresolvable session, or an unconfirmed pane
//! all resolve to `GuardFlags::default()` (`false`/`false`, guard fully
//! active), never a bypass. [`guard_flags`] is the production entry point
//! `pm_guard`'s Guards 2/3 call, reading [`MANAGED_SESSION_ID_ENV`] from the
//! live environment (the same var [`super::pm_guard_deny_by_default`]
//! reads) and this process's tmux pane id.
//!
//! Scope note: this ONLY covers managed sessions (`tm sessions new`/the
//! git-repo-backed `session start` path, and `tm sessions resume`) — the
//! dominant real-world path (the bare-`tm` guided default always routes a
//! GitHub-backed project through the managed session-manager). A LOCAL,
//! non-managed `tm sessions start` (a directory with no recognized git
//! remote) has no session-identity env var exported into its pane at all,
//! so [`MANAGED_SESSION_ID_ENV`] is simply absent there and both flags
//! resolve to their safe default — the guard is unconditionally fully
//! active for that path. That is a smaller footprint than the daemon's
//! local `SessionService`/`Session` plumbing would have required (see this
//! module's originating PR description for the fuller rationale) and is
//! never a regression: no session, managed or local, could disable the
//! guard via a live env var before this change either.
//! Test: `guard_flags_defaults_when_session_id_absent`,
//! `guard_flags_defaults_when_daemon_unreachable`,
//! `guard_flags_resolves_true_for_matching_pane`,
//! `guard_flags_defaults_when_pane_mismatch`,
//! `guard_flags_defaults_when_pane_unavailable`.

use super::pane_identity::EnvSessionIdentity;
use trusty_mpm::client::DaemonClient;

/// The pane env var a managed session's launcher exports — same constant
/// [`super::pm_guard_deny_by_default`] reads (kept as a private copy rather
/// than `pub(crate)`-exported from that module, since the two modules are
/// deliberately independent policy surfaces that happen to share one input).
const MANAGED_SESSION_ID_ENV: &str = "TM_MANAGED_SESSION_ID";

/// Resolved `pm_guard` kill-switch state.
///
/// Why: a small, explicit result type rather than a bare `(bool, bool)` tuple
/// keeps `pm_guard.rs`'s Guards 2/3 call sites self-documenting.
/// What: `Default` is `false`/`false` — guard fully active, the fail-safe
/// value returned for every unresolved case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct GuardFlags {
    pub(crate) disable_hooks: bool,
    pub(crate) pm_unrestricted: bool,
}

/// Best-effort daemon lookup of a managed session's guard flags AND its own
/// captured `pane_id` (mirrors
/// `pm_guard_deny_by_default::lookup_session_state_and_pane`).
///
/// What: `Some((flags, pane_id))` on a successful
/// `GET /api/v1/sessions/managed/{id}/guard-flags`; `None` on ANY failure —
/// client-build error, connect/timeout, non-2xx, or malformed body.
async fn lookup_guard_flags_and_pane(
    url: &str,
    session_id: &str,
) -> Option<(GuardFlags, Option<String>)> {
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(500))
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()?;
    let daemon = DaemonClient::with_client(client, url);
    let resp = daemon.get_guard_flags(session_id).await.ok()?;
    Some((
        GuardFlags {
            disable_hooks: resp.disable_hooks,
            pm_unrestricted: resp.pm_unrestricted,
        },
        resp.pane_id,
    ))
}

/// Resolve [`GuardFlags`] given an explicit (test-injectable) session id AND
/// this process's current tmux pane id (issue #3600 pane cross-check,
/// mirroring `pm_guard_deny_by_default::persona_status_for_session`).
///
/// What: no session id at all, a daemon-lookup failure, OR an unconfirmed
/// pane match ([`EnvSessionIdentity::evaluate`] not `is_confirmed()`) all
/// resolve to [`GuardFlags::default`] — fully active, never a bypass. Only a
/// CONFIRMED pane match trusts the resolved flags.
pub(crate) async fn guard_flags_for_session(
    raw_session_id: Option<&str>,
    current_pane_id: Option<&str>,
    url: &str,
) -> GuardFlags {
    let Some(session_id) = raw_session_id.map(str::trim).filter(|s| !s.is_empty()) else {
        return GuardFlags::default();
    };
    let Some((flags, record_pane_id)) = lookup_guard_flags_and_pane(url, session_id).await else {
        return GuardFlags::default();
    };
    if EnvSessionIdentity::evaluate(current_pane_id, record_pane_id.as_deref()).is_confirmed() {
        flags
    } else {
        // Mismatch or Unavailable: this pane cannot prove it owns
        // `session_id` — never trust a DIFFERENT session's recorded flags as
        // evidence for this one (the #3600 lesson, applied to the opposite
        // fail direction: unconfirmed here means "stay guarded", not
        // "allow").
        GuardFlags::default()
    }
}

/// Resolve [`GuardFlags`] for the CURRENT pane by reading
/// [`MANAGED_SESSION_ID_ENV`] and this process's own tmux `pane_id` from the
/// live environment.
///
/// Why: this is the production entry point `pm_guard`'s Guards 2/3 call.
pub(crate) async fn guard_flags(url: &str) -> GuardFlags {
    let raw = std::env::var(MANAGED_SESSION_ID_ENV).ok();
    let current_pane_id = super::tmux_attach::current_tmux_pane_id();
    guard_flags_for_session(raw.as_deref(), current_pane_id.as_deref(), url).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn guard_flags_defaults_when_session_id_absent() {
        let flags = guard_flags_for_session(None, None, "http://127.0.0.1:1").await;
        assert_eq!(flags, GuardFlags::default());
    }

    #[tokio::test]
    async fn guard_flags_defaults_when_daemon_unreachable() {
        // Fail toward guard-active: an unroutable daemon URL must resolve to
        // the default (fully active), never hang and never grant a bypass.
        let flags = guard_flags_for_session(
            Some("11111111-1111-1111-1111-111111111111"),
            Some("%5"),
            "http://127.0.0.1:1",
        )
        .await;
        assert_eq!(flags, GuardFlags::default());
    }

    /// Spawn a one-shot HTTP mock daemon replying to
    /// `GET /api/v1/sessions/managed/{id}/guard-flags` with a canned JSON
    /// body — mirrors `pm_guard_deny_by_default::tests::spawn_mock_daemon`.
    async fn spawn_mock_daemon(
        disable_hooks: bool,
        pm_unrestricted: bool,
        pane_id: &str,
    ) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let body = format!(
            r#"{{"disable_hooks":{disable_hooks},"pm_unrestricted":{pm_unrestricted},"pane_id":"{pane_id}"}}"#
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
    async fn guard_flags_resolves_true_for_matching_pane() {
        let url = spawn_mock_daemon(true, true, "%5").await;
        let flags = guard_flags_for_session(
            Some("11111111-1111-1111-1111-111111111111"),
            Some("%5"),
            &url,
        )
        .await;
        assert_eq!(
            flags,
            GuardFlags {
                disable_hooks: true,
                pm_unrestricted: true
            }
        );
    }

    #[tokio::test]
    async fn guard_flags_defaults_when_pane_mismatch() {
        // The daemon record says the bypass is on, but THIS pane ("%9") does
        // not match the record's pane ("%5") — must NOT inherit the bypass.
        let url = spawn_mock_daemon(true, true, "%5").await;
        let flags = guard_flags_for_session(
            Some("11111111-1111-1111-1111-111111111111"),
            Some("%9"),
            &url,
        )
        .await;
        assert_eq!(flags, GuardFlags::default());
    }

    #[tokio::test]
    async fn guard_flags_defaults_when_pane_unavailable() {
        // Not inside tmux (or the tmux query failed) — pane identity cannot
        // be verified. Must NOT trust the resolved (positively "on") flags.
        let url = spawn_mock_daemon(true, true, "%5").await;
        let flags =
            guard_flags_for_session(Some("11111111-1111-1111-1111-111111111111"), None, &url).await;
        assert_eq!(flags, GuardFlags::default());
    }
}
