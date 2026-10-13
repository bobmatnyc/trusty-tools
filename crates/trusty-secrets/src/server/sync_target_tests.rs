//! `secrets.copy` to a write-only sync target over real sockets (#9069).
//!
//! A child of `wire_tests`, so it shares that module's fixture and helpers:
//! every path is under a `TempDir`, the stores are in memory, and the socket
//! peer of every call is this test process, placed in a fake process table.
//!
//! Test: itself.

use super::*;

/// The GitHub Actions sync target's id on the wire.
const GITHUB: &str = "github-actions";

/// `copy` params from the fixture's Keychain double to `to`.
fn push_params(fx: &Fixture, to: &str, keys: &[&str]) -> Value {
    json!({"project": fx.project(), "from_backend": "keychain", "to_backend": to, "keys": keys})
}

/// Why: D5 — a push with no key names would publish every project key; it
/// is refused before any target opens. Red while `copy` treats an empty
/// list as "every key" for a sync target.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_copy_to_sync_target_without_keys_is_invalid_params() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _grants) = state_with(&fx, procs_with_me());
    let server = fx.start_state(state).await;
    let refused = call(
        &fx.settings.socket,
        method::COPY,
        push_params(&fx, GITHUB, &[]),
    )
    .await;
    server.stop().await;

    assert_eq!(
        fixed_error(&refused, method::COPY),
        ErrorKind::InvalidParams
    );
    assert_no_value(&refused);
}

/// Why: D3 (owner ruling D63) — a pushed value is readable by any workflow
/// a repository writer adds, so a caller under Claude Code may not push,
/// whatever the key's "agents may use" flag says. One deny record names the
/// target and the verdict. Red while the push path has no ancestry check.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_copy_to_sync_target_by_agent_descendant_is_refused_and_audited() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, true);
    let procs = procs_with_me();
    procs.set_agent(10);
    let (state, _grants) = state_with(&fx, procs);
    let server = fx.start_state(state).await;
    let refused = call(
        &fx.settings.socket,
        method::COPY,
        push_params(&fx, GITHUB, &["API_KEY"]),
    )
    .await;
    server.stop().await;

    assert_eq!(
        fixed_error(&refused, method::COPY),
        ErrorKind::AgentUseRefused
    );
    assert_no_value(&refused);
    let all = records(&fx);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(
        (all[0].method, all[0].decision),
        (AuditMethod::Copy, AuditDecision::Deny)
    );
    assert_eq!(
        all[0].reason.as_ref().map(AuditReason::as_str),
        Some("agent_use_refused")
    );
    assert_eq!(all[0].backend, Some(BackendId::new(GITHUB).unwrap()));
    assert_eq!(all[0].agent_parent, Some(true));
    assert_eq!(all[0].caller_pid, Some(me()));
}

/// Why: C8, D6 — a tracked project file may not aim a push at a repository
/// or project of its choosing, so a `github_actions:` or `vercel:` section
/// there is refused like the vendor CLI settings, naming the key only.
/// Red while those sections are ignored as unknown keys.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_tracked_sync_target_section_is_refused() {
    let fx = fixture();
    let config = fx.repo.join(PROJECT_CONFIG_SUBPATH);
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let server = fx.start().await;
    for section in ["github_actions", "vercel"] {
        std::fs::write(
            &config,
            format!("secrets:\n  {section}:\n    program: /tmp/{SENTINEL_VALUE}\n"),
        )
        .unwrap();
        let refused = call(
            &fx.settings.socket,
            method::SCOPES,
            json!({"project": fx.project()}),
        )
        .await;
        assert!(!wire(&refused).contains(SENTINEL_VALUE), "{section}");
        assert_eq!(
            fixed_error(&refused, method::SCOPES),
            ErrorKind::TrackedCliSettingRefused,
            "{section}"
        );
    }
    server.stop().await;
}
