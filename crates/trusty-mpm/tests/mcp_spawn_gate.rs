//! Integration tests for the MCP-initiated spawn gate (#1836, #1837).
//!
//! Why: the ARIA incident showed that an MCP `session_new` call reaches the
//! SAME [`spawn_managed`] engine the `tm` CLI uses, with no opt-in check —
//! letting an LLM-driven MCP call mint a real clone + worktree + tmux session
//! for ANY repo it names. These tests drive the actual daemon-level
//! `spawn_managed` entry point (not the pure gate logic, which is unit-tested
//! inline in `daemon::managed_routes::mcp_spawn_gate`) to prove: (1) a rejected
//! MCP-origin spawn creates NOTHING (no session record, no workspace) and (2)
//! a CLI/SM-STDIO-origin spawn (`mcp_initiated: false`) is NEVER gated, even
//! when MCP spawning is globally disabled (the default).
//!
//! No network and no clone: every case here is engineered to fail (or succeed)
//! before any side effect — either the gate itself short-circuits, or the
//! target is a local non-git directory that fails fast at "no git origin
//! remote", which is itself a strong signal the MCP gate was NOT the reason for
//! the failure.
//!
//! #6000 / ADR-0055: `repo_url` is a LOCAL DIRECTORY in every gated case here.
//! A remote URL no longer reaches the gate — `spawn_managed` refuses it first,
//! before anything else — so a remote-URL fixture would test the refusal rather
//! than the gate. The ARIA shape survives the change unaltered: an LLM naming an
//! arbitrary target it was never authorised to spawn, which is now a directory
//! path rather than a URL.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use serial_test::serial;
use tempfile::TempDir;

use trusty_mpm::daemon::managed_routes::{SpawnParams, spawn_managed};
use trusty_mpm::daemon::state::DaemonState;
use trusty_mpm::project::Project;

// #6671: `spawn_managed` reaches `DaemonState::project_registry()`, which seeds
// from the config file under `$HOME`, so an isolated framework root alone still
// admits the developer's registered projects.
mod common;

/// Env var the daemon reads to force-enable MCP spawning (mirrors
/// `daemon::managed_routes::mcp_spawn_gate::ALLOW_MCP_SPAWN_ENV`, duplicated
/// here as a literal so this test file has no dependency on that private
/// module).
const ALLOW_MCP_SPAWN_ENV: &str = "TRUSTY_MPM_ALLOW_MCP_SPAWN";

/// RAII guard that sets (or removes) `TRUSTY_MPM_ALLOW_MCP_SPAWN` for the
/// duration of a `#[serial]` test, restoring the prior value (or absence) on
/// drop — panic-safe, so a failed assertion mid-test cannot leak the override
/// into a sibling test (mirrors `session_launch::tests::EnvVarGuard`).
struct EnvGuard {
    prev: Option<String>,
}

impl EnvGuard {
    fn set(value: &str) -> Self {
        let prev = std::env::var(ALLOW_MCP_SPAWN_ENV).ok();
        // SAFETY: env-mutating tests using this guard are tagged `#[serial]`.
        unsafe { std::env::set_var(ALLOW_MCP_SPAWN_ENV, value) };
        Self { prev }
    }

    fn unset() -> Self {
        let prev = std::env::var(ALLOW_MCP_SPAWN_ENV).ok();
        // SAFETY: env-mutating tests using this guard are tagged `#[serial]`.
        unsafe { std::env::remove_var(ALLOW_MCP_SPAWN_ENV) };
        Self { prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: see `set`/`unset` — serialized by `#[serial]`.
        unsafe {
            match self.prev.take() {
                Some(v) => std::env::set_var(ALLOW_MCP_SPAWN_ENV, v),
                None => std::env::remove_var(ALLOW_MCP_SPAWN_ENV),
            }
        }
    }
}

fn base_params(repo_url: &str, mcp_initiated: bool) -> SpawnParams {
    SpawnParams {
        repo_url: repo_url.to_string(),
        git_ref: "main".to_string(),
        task: "test task".to_string(),
        name_hint: None,
        runtime: None,
        ephemeral: Some(true),
        mcp_initiated,
        inject_task: None,
        deliverable_id: None,
        force_new: false,
        // #5274: the gate runs before any placement decision, so these cases
        // are indifferent to it; `false` is the default every caller gets.
        worktree: false,
    }
}

/// A rejected MCP-initiated spawn (gate disabled by default) must create
/// NOTHING — no session record, no id, no workspace (#1836).
///
/// Why: the whole point of the off-by-default gate is that a refusal has zero
/// side effects; if a record were minted before the refusal, an operator would
/// still see orphan state accumulate even though the harness never launched.
/// What: calls `spawn_managed` with `mcp_initiated: true` against a fresh,
/// isolated `DaemonState` with `TRUSTY_MPM_ALLOW_MCP_SPAWN` unset (default
/// disabled), asserts the call errors mentioning "disabled", and asserts the
/// session manager's list is still empty afterward.
/// Test: this function IS the test.
#[tokio::test]
#[serial]
async fn mcp_initiated_spawn_rejected_by_default_creates_nothing() {
    let _env = EnvGuard::unset();

    common::scratch_home(); // #6671
    let root = TempDir::new().expect("root tempdir");
    let state = std::sync::Arc::new(
        DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await,
    );

    // #6000: a local directory, so the gate is what refuses — see the module doc.
    let target = TempDir::new().expect("target tempdir");
    let err = spawn_managed(
        &state,
        trusty_mpm::session_manager::ManagedSessionId::new(),
        base_params(&target.path().to_string_lossy(), true),
    )
    .await
    .expect_err("MCP-initiated spawn must be refused when spawning is disabled by default");
    assert!(err.contains("disabled"), "{err}");

    let mgr = state.session_manager().await;
    assert!(
        mgr.list().await.is_empty(),
        "a refused MCP spawn must create zero session records"
    );
}

/// An MCP-initiated spawn for an unregistered repo must be refused even when
/// spawning is explicitly enabled (#1837).
///
/// Why: the off-by-default toggle alone is not sufficient — the ARIA incident
/// happened from a trusty-tools session, so even an operator who enables MCP
/// spawning globally needs the second layer: the target repo must already be
/// KNOWN (registered), so a session in repo A cannot silently provision repo B.
/// What: enables spawning via the env var, leaves the registry empty, and
/// asserts the call errors mentioning "unregistered" with zero side effects.
/// Test: this function IS the test.
#[tokio::test]
#[serial]
async fn mcp_initiated_spawn_rejected_for_unregistered_repo_when_enabled() {
    let _env = EnvGuard::set("1");

    common::scratch_home(); // #6671
    let root = TempDir::new().expect("root tempdir");
    let state = std::sync::Arc::new(
        DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await,
    );

    // #6000: a local directory, so the gate is what refuses — see the module doc.
    let target = TempDir::new().expect("target tempdir");
    let err = spawn_managed(
        &state,
        trusty_mpm::session_manager::ManagedSessionId::new(),
        base_params(&target.path().to_string_lossy(), true),
    )
    .await
    .expect_err("an unregistered repo must be refused even when enabled");
    assert!(err.contains("unregistered"), "{err}");

    let mgr = state.session_manager().await;
    assert!(
        mgr.list().await.is_empty(),
        "a refused MCP spawn must create zero session records"
    );
}

/// CRITICAL regression (found in review): an attacker-controlled `repo_url`
/// that merely SHARES A REPO NAME with an already-registered project — but on
/// a different host/owner — must still be refused, even with MCP spawning
/// enabled (#1837).
///
/// Why: an earlier revision of the gate matched by bare derived repo name,
/// which let `https://evil.example.com/attacker/trusty-tools` impersonate a
/// registered `bobmatnyc/trusty-tools` project purely because the last path
/// segment ("trusty-tools") matched — reproducing the exact ARIA-incident
/// shape (an arbitrary, LLM-supplied `repo_url` gets cloned) through the
/// allowlist meant to prevent it. `spawn_managed` must reject this end-to-end.
/// What: registers a legitimate project, enables spawning, then targets the
/// impersonating URL and asserts the call is refused with zero side effects.
///
/// #6000 / ADR-0055: the refusal now comes from the local-path requirement
/// rather than the gate, because a remote URL no longer reaches the gate at
/// all. The end-to-end property this test exists for — an impersonating URL
/// spawns NOTHING — is unchanged and still proven here; the gate's own
/// owner+repo identity matching is covered by
/// `mcp_spawn_gate::tests::is_known_repo_rejects_same_repo_name_different_owner`,
/// which calls `is_known_repo` directly and is unaffected.
/// Test: this function IS the test.
#[tokio::test]
#[serial]
async fn mcp_initiated_spawn_rejects_repo_name_impersonation() {
    let _env = EnvGuard::set("1");

    common::scratch_home(); // #6671
    let root = TempDir::new().expect("root tempdir");
    let state = std::sync::Arc::new(
        DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await,
    );

    let registry = state.project_registry().await;
    registry
        .register(Project {
            name: "trusty-tools".to_string(),
            repo_url: "https://github.com/bobmatnyc/trusty-tools".to_string(),
            default_branch: "main".to_string(),
            stack_hint: None,
            tags: vec![],
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
            worktree: None,
        })
        .await
        .expect("register legitimate project");

    let err = spawn_managed(
        &state,
        trusty_mpm::session_manager::ManagedSessionId::new(),
        base_params("https://evil.example.com/attacker/trusty-tools", true),
    )
    .await
    .expect_err("a same-repo-name-different-owner URL must be refused, not impersonate");
    assert!(
        err.contains("ADR-0055"),
        "the impersonating URL must be refused outright; got: {err}"
    );

    let mgr = state.session_manager().await;
    assert!(
        mgr.list().await.is_empty(),
        "an impersonation attempt must create zero session records"
    );
}

/// An MCP-initiated spawn for an ALREADY-REGISTERED project proceeds past the
/// gate without extra ceremony (#1836, #1837 — the common case must not break).
///
/// Why: the gate must not punish the legitimate case — an operator who has
/// registered a project (or `tm` has auto-registered it from session history)
/// gets normal behaviour. This drives the target to a local, non-git temp
/// directory (never real network/git) so passing the gate is proven by
/// reaching the DIFFERENT downstream "no git origin remote" error rather than
/// the gate's own "disabled"/"unregistered" errors.
/// What: registers a project AT that local path — since #7066 the gate decides
/// on the canonical full path, so the registered `repo_url` is the checkout
/// itself (ADR-0055's supported `session_new` form), not a same-named directory
/// anywhere on disk — enables spawning, and asserts the resulting error is the
/// local-path no-origin error, NOT a gate refusal.
/// Test: this function IS the test.
#[tokio::test]
#[serial]
async fn mcp_initiated_spawn_allowed_for_registered_project_reaches_provisioning() {
    let _env = EnvGuard::set("1");

    common::scratch_home(); // #6671
    let root = TempDir::new().expect("root tempdir");
    let state = std::sync::Arc::new(
        DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await,
    );

    // A local, non-git directory registered BY ITS PATH — #7066: the gate
    // matches the canonical full path, so this exact directory is what the
    // registry has to name.
    let target_dir = TempDir::new().expect("target tempdir");
    let local_path = target_dir.path().join("known-project");
    std::fs::create_dir(&local_path).expect("create target dir");

    let registry = state.project_registry().await;
    registry
        .register(Project {
            name: "known-project".to_string(),
            repo_url: local_path.to_string_lossy().into_owned(),
            default_branch: "main".to_string(),
            stack_hint: None,
            tags: vec![],
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
            worktree: None,
        })
        .await
        .expect("register project");

    let err = spawn_managed(
        &state,
        trusty_mpm::session_manager::ManagedSessionId::new(),
        base_params(&local_path.to_string_lossy(), true),
    )
    .await
    .expect_err("non-git local dir must fail downstream, not at the gate");
    assert!(
        err.contains("no git origin remote"),
        "expected the gate to pass through to the local-path branch, got: {err}"
    );
    assert!(
        !err.contains("disabled") && !err.contains("unregistered"),
        "the MCP gate must not be why this failed: {err}"
    );
}

/// #7066: a directory that merely shares a BASENAME with a registered project,
/// but lives outside it, must be refused end to end.
///
/// Why: the gate's local arm used to compare the target's last path segment
/// against each registered project's `name`, so any directory named like a
/// registered project satisfied the allowlist — and ADR-0055 left that as the
/// only rule an MCP-initiated spawn reaches, because a remote `repo_url` is now
/// refused before the gate runs.
/// What: registers a project at one real directory, then targets a DIFFERENT
/// directory with the same basename and asserts `spawn_managed` refuses it as
/// unregistered with zero session records.
/// Test: this function IS the test.
#[tokio::test]
#[serial]
async fn mcp_initiated_spawn_rejects_a_same_basename_directory_outside_the_project() {
    let _env = EnvGuard::set("1");

    common::scratch_home(); // #6671
    let root = TempDir::new().expect("root tempdir");
    let state = std::sync::Arc::new(
        DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await,
    );

    let fixture = TempDir::new().expect("fixture tempdir");
    let registered = fixture.path().join("checkouts").join("known-project");
    let impostor = fixture.path().join("elsewhere").join("known-project");
    std::fs::create_dir_all(&registered).expect("create the registered checkout");
    std::fs::create_dir_all(&impostor).expect("create the same-named impostor");

    let registry = state.project_registry().await;
    registry
        .register(Project {
            name: "known-project".to_string(),
            repo_url: registered.to_string_lossy().into_owned(),
            default_branch: "main".to_string(),
            stack_hint: None,
            tags: vec![],
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
            worktree: None,
        })
        .await
        .expect("register the legitimate checkout");

    let err = spawn_managed(
        &state,
        trusty_mpm::session_manager::ManagedSessionId::new(),
        base_params(&impostor.to_string_lossy(), true),
    )
    .await
    .expect_err("a same-basename directory outside the project must be refused");
    assert!(err.contains("unregistered"), "{err}");

    let mgr = state.session_manager().await;
    assert!(
        mgr.list().await.is_empty(),
        "a refused MCP spawn must create zero session records"
    );
}

/// A CLI/SM-STDIO-origin spawn (`mcp_initiated: false`) is NEVER subject to the
/// MCP gate, even with MCP spawning left at its disabled default (#1836).
///
/// Why: `tm launch`/`tm connect`/`tm ticket` and the SM-STDIO `sm.sessions.launch`
/// path must keep working exactly as before this change — the gate is scoped
/// STRICTLY to the MCP tool surface.
/// What: same local non-git directory trick as above, with `mcp_initiated:
/// false` and the gate left disabled; asserts the error is the downstream
/// no-git-origin error, never a gate refusal.
/// Test: this function IS the test.
#[tokio::test]
#[serial]
async fn cli_origin_spawn_bypasses_mcp_gate_even_when_disabled() {
    let _env = EnvGuard::unset();

    common::scratch_home(); // #6671
    let root = TempDir::new().expect("root tempdir");
    let state = std::sync::Arc::new(
        DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await,
    );

    let target_dir = TempDir::new().expect("target tempdir");
    let local_path = target_dir.path().join("unregistered-cli-project");
    std::fs::create_dir(&local_path).expect("create target dir");

    let err = spawn_managed(
        &state,
        trusty_mpm::session_manager::ManagedSessionId::new(),
        base_params(&local_path.to_string_lossy(), false),
    )
    .await
    .expect_err("non-git local dir must fail downstream, not at the gate");
    assert!(
        err.contains("no git origin remote"),
        "expected the CLI-origin spawn to bypass the gate entirely, got: {err}"
    );
}
