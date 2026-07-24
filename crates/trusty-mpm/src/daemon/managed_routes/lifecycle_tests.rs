//! Tests for `daemon::managed_routes::lifecycle` (managed-session spawn,
//! reconnect, and deployment-completeness logic).
//!
//! Why: split out of the large inline `#[cfg(test)] mod tests` in
//! `lifecycle.rs` (issue #3312 review follow-up) purely to keep
//! `lifecycle.rs` — a production file already grandfathered on the
//! `.line-cap-allowlist.tsv` SLOC ratchet — from growing further: this file
//! is classified as a test file (1500 SLOC cap) by its `_tests.rs` suffix,
//! mirroring the exact convention `core::gh_account_spawn_env_tests`
//! established for the same reason. Pure code motion — no behavior change;
//! every test below is verbatim from `lifecycle.rs`'s former inline module.
//! What: `StubTmux` + `stub_record` fixtures and the
//! `find_reusable_inproject_session_*`/`reconnect_candidate_*`/
//! `prepare_inproject_session_*`/`reserve_inproject_worktree_*`/
//! `ensure_deployment_complete_*`/`carrier_reachable_*`/
//! `warn_if_no_persona_carrier_*` test groups.
//! Test: this file IS the test module.

use super::*;

/// Minimal `ManagedTmuxDriver` test double scoped to this module.
///
/// Why (issue #1931): [`find_reusable_inproject_session`] only needs
/// `session_exists`, so this fake needs no real tmux process — just a
/// settable list of session names considered "alive". The crate's other
/// `FakeTmuxDriver` (`session_manager::tests`) is not reachable from here
/// (it lives in a private sibling module), so a tiny local double is
/// simpler than threading visibility through the module tree.
/// What: `session_exists` returns `true` iff `name` is in `alive`; every
/// other trait method is unused by this module's tests and panics if
/// called, so a wiring mistake fails loudly instead of silently passing.
/// Test: used by the `find_reusable_inproject_session_*` tests below.
struct StubTmux {
    alive: Vec<String>,
}

impl crate::session_manager::ManagedTmuxDriver for StubTmux {
    fn create_session(&self, _name: &str, _workdir: &str) -> Result<(), ManagedError> {
        unimplemented!("not exercised by find_reusable_inproject_session tests")
    }
    fn kill_session(&self, _name: &str) -> Result<(), ManagedError> {
        unimplemented!("not exercised by find_reusable_inproject_session tests")
    }
    fn send_line(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
        unimplemented!("not exercised by find_reusable_inproject_session tests")
    }
    fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
        unimplemented!("not exercised by find_reusable_inproject_session tests")
    }
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(self.alive.clone())
    }
}

/// Builds a minimal [`SessionRecord`] for [`find_reusable_inproject_session`]
/// tests — only `source_id`, `state`, and `tmux_name` affect the predicate;
/// every other field is an arbitrary placeholder.
///
/// `#[rustfmt::skip]`: the trailing always-placeholder fields are
/// deliberately paired up two-per-line — this file is grandfathered at a
/// frozen SLOC budget (`.line-cap-allowlist.tsv`, #2364), so a
/// one-line-per-field expansion here would ratchet it up.
#[rustfmt::skip]
fn stub_record(
    source_id: Option<&str>,
    state: ManagedSessionState,
    tmux_name: &str,
) -> SessionRecord {
    SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: tmux_name.to_owned(),
        cwd: std::path::PathBuf::from("/tmp/project"),
        task: "task".into(),
        state,
        created_at: chrono::Utc::now(),
        last_activity_at: None,
        workspace_path: None, repo_url: None,
        branch: None, pending_decision: None,
        proposed_default: None, correlation: Default::default(),
        runtime: Default::default(),
        ephemeral: false, workspace_owned: false,
        source_id: source_id.map(str::to_owned),
        claude_session_id: None, scrollback_path: None,
        last_cwd: None, deliverable_id: None,
        pane_id: None, injection_status: Default::default(),
            worktree_owner: None,
    }
}

/// Issue #1931 regression guard (symptom 1 investigation): proves the
/// exact predicate `tm` relies on to reconnect to an already-provisioned
/// managed project instead of spawning a duplicate clone/worktree — an
/// Active record with a matching `source_id` AND a still-live tmux
/// session must be returned.
#[test]
fn find_reusable_inproject_session_matches_active_live_session() {
    let records = vec![stub_record(
        Some("bobmatnyc/trusty-tools"),
        ManagedSessionState::Active,
        "tmpm-trusty-tools-abc123",
    )];
    let tmux = StubTmux {
        alive: vec!["tmpm-trusty-tools-abc123".to_owned()],
    };

    let found = find_reusable_inproject_session(&records, "bobmatnyc/trusty-tools", &tmux);

    assert!(
        found.is_some(),
        "an Active record with a live tmux session for the same source_id must be reused"
    );
    assert_eq!(found.unwrap().tmux_name, "tmpm-trusty-tools-abc123");
}

/// Issue #1931: three ways the predicate must correctly say "no reusable
/// session" — a different project's source_id, a non-Active state (e.g.
/// `Stopped`, matching the real symptom-1 investigation where prior
/// sessions were `state=stopped`), and a record whose tmux session has
/// died. Any of these incorrectly matching would either miss a reconnect
/// opportunity or, worse, hand back a dead session record.
#[test]
fn find_reusable_inproject_session_ignores_stopped_or_dead_or_other_project() {
    let records = vec![
        stub_record(
            Some("bobmatnyc/xflux"),
            ManagedSessionState::Active,
            "tmpm-xflux-live",
        ),
        stub_record(
            Some("bobmatnyc/trusty-tools"),
            ManagedSessionState::Stopped,
            "tmpm-trusty-tools-stopped",
        ),
        stub_record(
            Some("bobmatnyc/trusty-tools"),
            ManagedSessionState::Active,
            "tmpm-trusty-tools-dead-tmux",
        ),
    ];
    let tmux = StubTmux {
        alive: vec!["tmpm-xflux-live".to_owned()],
    };

    let found = find_reusable_inproject_session(&records, "bobmatnyc/trusty-tools", &tmux);

    assert!(
        found.is_none(),
        "must not reuse a different project's session, a Stopped record, \
         or an Active record whose tmux session is no longer alive; got: {found:?}"
    );
}

/// #2450: `force_new = true` must SKIP the reconnect entirely — even when a
/// perfectly reusable Active+live session for the same project exists. This
/// is the exact opt-out the picker's "launch new session" choice relies on
/// so it can never inject its task into an unrelated live session.
#[test]
fn reconnect_candidate_none_when_force_new() {
    let records = vec![stub_record(
        Some("bobmatnyc/trusty-tools"),
        ManagedSessionState::Active,
        "tmpm-trusty-tools-abc123",
    )];
    let tmux = StubTmux {
        alive: vec!["tmpm-trusty-tools-abc123".to_owned()],
    };

    // Sanity: without force_new this same input DOES reconnect (below).
    let forced = reconnect_candidate(true, &records, "bobmatnyc/trusty-tools", &tmux);

    assert!(
        forced.is_none(),
        "force_new must skip the reconnect even when a live session exists"
    );
}

/// #2450 companion: `force_new = false` must PRESERVE the #1707 reconnect —
/// `reconnect_candidate` delegates to the unchanged predicate, so an
/// Active+live session for the same project is still adopted. Guards against
/// the opt-out accidentally disabling reconnect for the default path.
#[test]
fn reconnect_candidate_reconnects_when_not_forced() {
    let records = vec![stub_record(
        Some("bobmatnyc/trusty-tools"),
        ManagedSessionState::Active,
        "tmpm-trusty-tools-abc123",
    )];
    let tmux = StubTmux {
        alive: vec!["tmpm-trusty-tools-abc123".to_owned()],
    };

    let found = reconnect_candidate(false, &records, "bobmatnyc/trusty-tools", &tmux);

    assert_eq!(
        found.map(|r| r.tmux_name),
        Some("tmpm-trusty-tools-abc123".to_owned()),
        "without force_new the #1707 reconnect must still adopt a live session"
    );
}

/// #1913 regression guard: [`prepare_inproject_session`] — the call this fix
/// adds to [`spawn_managed_inproject`] BEFORE `adapter.spawn` — must actually
/// run the preparation pipeline and land its most visible symptom (the
/// reported bug): the `statusLine` key in `<worktree>/.claude/settings.json`.
///
/// Why hermetic: `spawn_managed_inproject` itself needs a live `DaemonState`
/// (tmux driver, session store) plus a real git worktree from
/// `try_inproject_spawn`, which the crate's existing test suite deliberately
/// avoids driving end-to-end (see `handler_spawn_wires_provision_and_spawn`'s
/// comment in `tests/session_manager_mvp.rs` — replicating handler steps
/// rather than calling the private handler). `prepare_inproject_session` was
/// extracted specifically so the ONE new call this fix adds is independently
/// testable: point `FrameworkPaths::under` at a tempdir (never the operator's
/// real `~/.trusty-mpm`/`~/.claude`) and call it directly against a plain
/// temp directory standing in for the worktree — no daemon, tmux, or git
/// required, matching how `session_launch::tests` already exercises
/// `prepare_session*` hermetically.
/// What: calls `prepare_inproject_session` with a hermetic `fw` and a fresh
/// temp "worktree" dir, then asserts `<worktree>/.claude/settings.json`
/// exists and contains `"statusLine"` — proving the prep pipeline actually
/// ran (before this fix, nothing in `spawn_managed_inproject` ever wrote
/// this file).
/// Test: this function IS the test.
#[test]
fn prepare_inproject_session_writes_statusline() {
    let tmp_home = tempfile::TempDir::new().expect("tmp home");
    let worktree = tempfile::TempDir::new().expect("tmp worktree");
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());
    let session_id = ManagedSessionId::new();

    prepare_inproject_session(
        &fw,
        &session_id,
        worktree.path(),
        "https://github.com/owner/repo",
    );

    let settings_path = worktree.path().join(".claude").join("settings.json");
    let content = std::fs::read_to_string(&settings_path).unwrap_or_else(|e| {
        panic!(
            "prepare_inproject_session must write {}: {e}",
            settings_path.display()
        )
    });
    assert!(
        content.contains("statusLine"),
        "prepared worktree settings.json must carry the statusLine key \
         (the #1913 symptom); got: {content}"
    );
}

/// #1919 regression guard: [`spawn_managed_inproject`]'s call tree —
/// specifically [`prepare_inproject_session`] → `prepare_session_with_repo_url`
/// → `prepare_session_inner` — must emit its `DeployingAgents`/
/// `DeployingSkills`/`BuildingInstructions`/`ConfiguringMcp` stage events
/// when a [`crate::core::provisioning_stage::StageEmitter`] scope is
/// active. Before #1919, `spawn_managed`'s `is_local_workdir` branch
/// (which routes to `spawn_managed_inproject`) returned BEFORE the scope
/// was ever installed, so these `emit(...)` calls fired into the void for
/// every in-project spawn — the dominant path since #1916.
///
/// Why hermetic: same rationale as
/// `prepare_inproject_session_writes_statusline` above —
/// `spawn_managed_inproject` needs a live `DaemonState`/tmux/git worktree
/// the crate's test suite deliberately avoids driving end-to-end, but
/// `prepare_inproject_session` is the one new call #1913 added to that
/// function's call tree, and it is independently testable against a
/// hermetic `FrameworkPaths::under` tempdir plus a plain temp directory
/// standing in for the worktree — mirroring
/// `session_launch::tests::prepare_session_emits_stage_events_in_order`,
/// which proves the identical emit sites fire correctly on the
/// clone-based path.
/// What: wraps `prepare_inproject_session` in a `scoped(...)` backed by a
/// fresh broadcast channel, drains every event it emitted, and asserts
/// the four `session_launch`-owned stages appear, IN ORDER. This is the
/// same call path `spawn_managed_inproject` now exercises for real once
/// #1919 moved the `StageEmitter` scope up to cover the in-project branch.
/// Test: this function IS the test.
#[tokio::test]
async fn prepare_inproject_session_emits_stage_events_in_order() {
    use crate::core::provisioning_stage::{ProvisioningStage, StageEmitter, scoped};

    let tmp_home = tempfile::TempDir::new().expect("tmp home");
    let worktree = tempfile::TempDir::new().expect("tmp worktree");
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());
    let session_id = ManagedSessionId::new();

    let (tx, mut rx) = tokio::sync::broadcast::channel(32);
    let emitter = StageEmitter::new(session_id.to_string(), "https://github.com/owner/repo", tx);

    scoped(emitter, async {
        prepare_inproject_session(
            &fw,
            &session_id,
            worktree.path(),
            "https://github.com/owner/repo",
        );
    })
    .await;

    let mut stages = Vec::new();
    while let Ok(value) = rx.try_recv() {
        assert_eq!(value["kind"], "provisioning_stage");
        assert_eq!(value["repo_url"], "https://github.com/owner/repo");
        stages.push(value["stage"].as_str().unwrap().to_string());
    }

    assert_eq!(
        stages,
        vec![
            ProvisioningStage::DeployingAgents.wire_name(),
            ProvisioningStage::DeployingSkills.wire_name(),
            ProvisioningStage::BuildingInstructions.wire_name(),
            ProvisioningStage::ConfiguringMcp.wire_name(),
        ],
        "prepare_inproject_session's call tree must emit exactly these \
         four stages, in order, when a StageEmitter scope is active"
    );
}

/// Issue #2032: [`reserve_inproject_worktree`] must name the per-session
/// worktree/branch after the SEMANTIC tmux name (`tm-<repo>-NN`), not the
/// raw session UUID — and the returned name must be the exact name used
/// for both the worktree directory and (via `create_session_worktree`)
/// the `session/<name>` branch.
///
/// Why hermetic: `DaemonState::with_root_isolated_managed` (the same
/// helper `tests/session_manager_mvp.rs` uses) gives this test a real
/// `SessionManager` backed by `FakeNoopTmuxDriver` — no real tmux, no
/// production store — while a real temp git repo stands in for the base
/// clone so `create_session_worktree`'s `git worktree add` actually runs.
/// What: builds a real git repo (init + one commit) as `base`, calls
/// `reserve_inproject_worktree` with `repo = "trusty-tools"`, and asserts
/// (a) the resolved name matches `tm-trusty-tools-01` (NOT a UUID); (b)
/// the returned worktree path ends with that exact name; (c) the
/// worktree directory actually exists on disk.
/// Test: this function IS the test.
#[tokio::test]
async fn reserve_inproject_worktree_uses_semantic_name_not_uuid() {
    let data_root = tempfile::TempDir::new().expect("tmp data root");
    let state = std::sync::Arc::new(
        crate::daemon::state::DaemonState::with_root_isolated_managed(
            data_root.path().to_path_buf(),
        )
        .await,
    );

    let base_dir = tempfile::TempDir::new().expect("tmp base dir");
    let base = base_dir.path().to_path_buf();
    assert!(
        std::process::Command::new("git")
            .arg("init")
            .current_dir(&base)
            .status()
            .map(|s| s.success())
            .unwrap_or(false),
        "git init must succeed in this test fixture"
    );
    for (k, v) in [("user.email", "t@example.com"), ("user.name", "T")] {
        let _ = std::process::Command::new("git")
            .args(["-C", base.to_str().unwrap(), "config", k, v])
            .status();
    }
    assert!(
        std::process::Command::new("git")
            .args([
                "-C",
                base.to_str().unwrap(),
                "commit",
                "--allow-empty",
                "-m",
                "init",
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false),
        "git commit must succeed in this test fixture"
    );

    let session_id = ManagedSessionId::new();
    let params = SpawnParams {
        repo_url: base.to_string_lossy().into_owned(),
        git_ref: "main".into(),
        task: "task".into(),
        name_hint: None,
        runtime: None,
        ephemeral: Some(true),
        mcp_initiated: false,
        inject_task: None,
        deliverable_id: None,
        force_new: false,
    };

    let config = crate::core::trusty_tools_config::TrustyToolsConfig::default();
    let (worktree, reserved_name) = reserve_inproject_worktree(
        &state,
        &session_id,
        &params,
        &base,
        &base,
        "trusty-tools",
        &config,
    )
    .await
    .expect("reserve_inproject_worktree must succeed against a real git repo");

    assert_eq!(
        reserved_name, "tm-trusty-tools-01",
        "the resolved name must be the SEMANTIC tm-<repo>-NN form, not the raw session UUID"
    );
    assert!(
        worktree.ends_with(&reserved_name),
        "the worktree path must end with the resolved semantic name, got {}",
        worktree.display()
    );
    assert!(
        !worktree.to_string_lossy().contains(&session_id.to_string()),
        "the worktree path must NOT contain the raw session UUID (issue #2032), got {}",
        worktree.display()
    );
    assert!(
        worktree.is_dir(),
        "the worktree directory must exist on disk, got {}",
        worktree.display()
    );
}

/// Why (#2158): the adopted-session sentinel `/unknown` (and any
/// non-existent workspace) must never be handed to `validate_and_repair`
/// — there is nothing on disk to diff, and the repair pipeline would
/// fail trying to `create_dir_all` under it. The gate must silently no-op
/// instead.
/// Test: itself.
#[test]
fn ensure_deployment_complete_noops_for_unknown_workspace() {
    // `fw`'s fields are never dereferenced on this early-return path, so
    // a fixed placeholder base (no I/O, no tempdir) is sufficient.
    let id = ManagedSessionId::new();
    let fw = crate::core::paths::FrameworkPaths::under("/nonexistent-fw-base-for-test");
    let result = ensure_deployment_complete(&fw, std::path::Path::new("/unknown"), None, &id);
    assert!(result.is_ok());

    let missing = std::path::Path::new("/this/path/does/not/exist/anywhere");
    let result = ensure_deployment_complete(&fw, missing, None, &id);
    assert!(result.is_ok());
}

/// Why (#2158): a workspace whose `.claude/` payload already matches the
/// canonical roster must pass the gate without attempting a repair.
/// Test: itself.
#[test]
fn ensure_deployment_complete_ok_when_already_complete() {
    use crate::core::agent_manifest::AgentManifest;
    use crate::core::paths::FrameworkPaths;
    use crate::core::skill_manifest::SkillManifest;

    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    // Fully hermetic: `trusty_mpm_root = None` forces the canonical SOURCE
    // dirs to resolve under the temp `fw.agents`/`fw.skills` (empty here),
    // never the real daemon-default `~/.trusty-mpm` — so this test's
    // verdict cannot depend on what happens to be installed on the
    // machine running it. An empty canonical roster plus a fully
    // manifested + settings-configured target is "complete" by
    // definition (nothing to diff against).
    let mut fw = FrameworkPaths::for_managed_project(tmp.path(), &workspace);
    fw.trusty_mpm_root = None;
    let agents_dir = fw.claude_agents_dir();
    std::fs::create_dir_all(&agents_dir).unwrap();
    AgentManifest::default().save(&agents_dir).unwrap();
    let skills_dir = fw.claude_skills_dir();
    std::fs::create_dir_all(&skills_dir).unwrap();
    SkillManifest::default().save(&skills_dir).unwrap();

    let claude_dir = workspace.join(".claude");
    std::fs::write(
        claude_dir.join("settings.json"),
        r#"{"outputStyle": "trusty-mpm", "hooks": {"SessionStart": []}}"#,
    )
    .unwrap();
    let style_dir = claude_dir.join("output-styles");
    std::fs::create_dir_all(&style_dir).unwrap();
    let default_style = crate::core::bundle::OUTPUT_STYLES[0];
    std::fs::write(
        style_dir.join(default_style.file_name),
        default_style.content,
    )
    .unwrap();

    let id = ManagedSessionId::new();
    let result = ensure_deployment_complete(&fw, &workspace, None, &id);
    assert!(result.is_ok(), "expected Ok, got {result:?}");
}

/// Why (#2231): when neither carrier is present (no output-style gap
/// resolved AND no prompt-file stash), an empty `gaps` slice trivially
/// satisfies the output-style branch — this proves that specific
/// short-circuit.
/// Test: itself.
#[test]
fn carrier_reachable_true_when_no_output_style_gap() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(
        carrier_reachable(&[], tmp.path()),
        "no output-style gap at all must be treated as carrier-reachable"
    );
}

/// Why (#2231): the prompt-file carrier is an ALTERNATIVE to the
/// output-style carrier — a workspace with an output-style gap but a
/// present, non-empty `.trusty-mpm/last-instructions.md` must still be
/// reachable.
/// Test: itself.
#[test]
fn carrier_reachable_true_when_prompt_file_present_despite_style_gap() {
    let tmp = tempfile::tempdir().unwrap();
    let stash_dir = tmp.path().join(".trusty-mpm");
    std::fs::create_dir_all(&stash_dir).unwrap();
    std::fs::write(stash_dir.join("last-instructions.md"), "you are the PM").unwrap();

    let gaps = vec![crate::core::deploy_validate::DeploymentGap::OutputStyleKeyMissing];
    assert!(
        carrier_reachable(&gaps, tmp.path()),
        "a present, non-empty prompt-file stash must satisfy the carrier check \
         even when the output-style carrier has a gap"
    );
}

/// Why (#2231): the false case — an output-style gap AND no prompt-file
/// stash at all — must resolve to "no carrier reachable" so the warn-only
/// diagnostic fires.
/// Test: itself.
#[test]
fn carrier_reachable_false_when_neither_carrier_present() {
    let tmp = tempfile::tempdir().unwrap();
    let gaps = vec![
        crate::core::deploy_validate::DeploymentGap::OutputStyleFileMissing(
            "trusty-mpm".to_string(),
        ),
    ];
    assert!(
        !carrier_reachable(&gaps, tmp.path()),
        "an output-style gap with no prompt-file stash must be unreachable"
    );
}

/// Why (#2231): an EMPTY (zero-byte) prompt-file stash must not count as
/// "wired" — a truncated/placeholder file is not a real carrier.
/// Test: itself.
#[test]
fn carrier_reachable_false_when_prompt_file_present_but_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let stash_dir = tmp.path().join(".trusty-mpm");
    std::fs::create_dir_all(&stash_dir).unwrap();
    std::fs::write(stash_dir.join("last-instructions.md"), "").unwrap();

    let gaps = vec![
        crate::core::deploy_validate::DeploymentGap::OutputStyleUnknownId("bogus".to_string()),
    ];
    assert!(!carrier_reachable(&gaps, tmp.path()));
}

/// Why (#2231): [`warn_if_no_persona_carrier`] returns `()` and is the
/// ONLY thing `ensure_deployment_complete` calls for this self-check — its
/// signature already makes it structurally impossible to turn the
/// caller's `Ok` into an `Err`. This proves it also never PANICS when
/// neither carrier is reachable (the exact condition that makes it log).
/// Test: itself — reaching the end of this test without panicking IS the
/// assertion.
#[test]
fn warn_if_no_persona_carrier_does_not_panic_when_neither_carrier_present() {
    let tmp = tempfile::tempdir().unwrap();
    let gaps = vec![crate::core::deploy_validate::DeploymentGap::OutputStyleKeyMissing];
    let id = ManagedSessionId::new();
    warn_if_no_persona_carrier(&gaps, tmp.path(), &id);
}

/// Why (#2231): full-pipeline regression guard — even when auto-repair
/// cannot write anything at all (workspace directory made read-only, so
/// NEITHER the output-style file nor the `.trusty-mpm/last-instructions.md`
/// stash can be created), `ensure_deployment_complete` must still RETURN
/// (not panic/hang) — the carrier self-check is purely additive logging
/// and can never abort this call. The pre-existing (unrelated, #2172)
/// contract — an unrepairable gap surfaces as `Err` for the caller to log
/// non-blockingly — is asserted too, proving this diagnostic didn't change
/// it. Skipped when running as root: a read-only directory does not block
/// root's writes, so the "nothing got written" precondition cannot be
/// established.
/// Test: itself. Unix-only (permission bits).
#[cfg(unix)]
#[test]
fn ensure_deployment_complete_does_not_abort_when_no_carrier_reachable() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::set_permissions(&workspace, std::fs::Permissions::from_mode(0o555)).unwrap();

    // Confirm the read-only precondition actually holds before relying on
    // it — running as root would silently defeat it.
    let probe = workspace.join(".probe");
    if std::fs::write(&probe, "x").is_ok() {
        let _ = std::fs::remove_file(&probe);
        let _ = std::fs::set_permissions(&workspace, std::fs::Permissions::from_mode(0o755));
        eprintln!(
            "skipping ensure_deployment_complete_does_not_abort_when_no_carrier_reachable: \
             read-only directory did not block a write (likely running as root)"
        );
        return;
    }

    let mut fw = crate::core::paths::FrameworkPaths::for_managed_project(tmp.path(), &workspace);
    fw.trusty_mpm_root = None;
    let id = ManagedSessionId::new();

    let result = ensure_deployment_complete(&fw, &workspace, None, &id);

    // Restore write permission so the TempDir can clean itself up.
    let _ = std::fs::set_permissions(&workspace, std::fs::Permissions::from_mode(0o755));

    assert!(
        result.is_err(),
        "expected the pre-existing incomplete-after-repair contract to still hold \
         (unrelated to the new carrier self-check); got {result:?}"
    );
}

// ── #3455 "launch on main" opt-out ──────────────────────────────────────────
//
// The functions under test moved out of `lifecycle` into the sibling
// `launch_on_main` module (500-SLOC cap on `lifecycle.rs`); their tests stay
// here with the rest of the spawn-surface tests and reach them via the
// managed_routes-scoped `pub(super)` path.
use super::super::launch_on_main::{
    has_concurrent_main_checkout_session, spawn_managed_on_main, worktree_enabled_for_origin,
};

/// A project with no registry entry (or no `worktree` field set) defaults to
/// worktree isolation ON — the no-regression default #3455 requires.
/// Test: itself.
#[tokio::test]
async fn worktree_enabled_for_origin_defaults_true_when_unregistered() {
    let data_root = tempfile::TempDir::new().expect("tmp data root");
    let state = crate::daemon::state::DaemonState::with_root_isolated_managed(
        data_root.path().to_path_buf(),
    )
    .await;
    let registry = state.project_registry().await;

    assert!(
        worktree_enabled_for_origin(&registry, "https://github.com/acme/unregistered").await,
        "an unregistered repo must default to worktree isolation ON"
    );
}

/// A registered project with `worktree: Some(false)` disables isolation for
/// its own `repo_url` only — a DIFFERENT repo is unaffected (#3455).
/// Test: itself.
#[tokio::test]
async fn worktree_enabled_for_origin_honors_registered_false() {
    let data_root = tempfile::TempDir::new().expect("tmp data root");
    let state = crate::daemon::state::DaemonState::with_root_isolated_managed(
        data_root.path().to_path_buf(),
    )
    .await;
    let registry = state.project_registry().await;

    registry
        .register(crate::project::Project {
            name: "writing".into(),
            repo_url: "https://github.com/bobmatnyc/writing".into(),
            default_branch: "main".into(),
            stack_hint: None,
            tags: vec![],
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
            worktree: Some(false),
        })
        .await
        .expect("register writing project");

    assert!(
        !worktree_enabled_for_origin(&registry, "https://github.com/bobmatnyc/writing.git").await,
        "the registered project's opt-out must be honored (repo_url matching tolerates .git suffix)"
    );
    assert!(
        worktree_enabled_for_origin(&registry, "https://github.com/bobmatnyc/trusty-tools").await,
        "a DIFFERENT repo must be unaffected by another project's opt-out"
    );
}

/// `spawn_managed_on_main` creates a normal `Active` session record rooted
/// DIRECTLY at `local_path` — no worktree, no clone, `workspace_owned =
/// false` so decommission never auto-deletes the operator's own checkout
/// (#3455).
/// Test: itself.
#[tokio::test]
async fn spawn_managed_on_main_creates_record_without_worktree() {
    let data_root = tempfile::TempDir::new().expect("tmp data root");
    let state = std::sync::Arc::new(
        crate::daemon::state::DaemonState::with_root_isolated_managed(
            data_root.path().to_path_buf(),
        )
        .await,
    );

    // The operator's own checkout — a real git repo, used directly, not cloned.
    let checkout = tempfile::TempDir::new().expect("tmp checkout dir");
    let local_path = checkout.path();
    assert!(
        std::process::Command::new("git")
            .arg("init")
            .current_dir(local_path)
            .status()
            .map(|s| s.success())
            .unwrap_or(false),
        "git init must succeed in this test fixture"
    );

    let session_id = ManagedSessionId::new();
    let params = SpawnParams {
        repo_url: local_path.to_string_lossy().into_owned(),
        git_ref: "main".into(),
        task: "".into(),
        name_hint: None,
        runtime: None,
        ephemeral: Some(true),
        mcp_initiated: false,
        inject_task: None,
        deliverable_id: None,
        force_new: false,
    };

    let record = spawn_managed_on_main(
        &state,
        &session_id,
        &params,
        crate::runtime::RuntimeKind::ClaudeCode,
        local_path,
        "acme",
        "writing",
    )
    .await
    .expect("spawn_managed_on_main must succeed against a real git repo");

    assert_eq!(
        record.cwd, local_path,
        "the session cwd must be the main checkout itself, not a worktree"
    );
    assert_eq!(
        record.workspace_path.as_deref(),
        Some(local_path),
        "workspace_path must point at the main checkout"
    );
    assert!(
        !record.workspace_owned,
        "workspace_owned must be false — decommission must never auto-delete \
         the operator's own checkout"
    );
    assert!(
        !local_path.join(".worktrees").exists(),
        "no .worktrees/ directory must ever be created for a launch-on-main session"
    );
    assert_eq!(
        record.source_id.as_deref(),
        Some("acme/writing"),
        "source_id must still be set so reconnect works normally"
    );
}

/// The concurrent-collision detector `spawn_managed_on_main` warns on (#3455)
/// fires ONLY for an already-`Active` session whose cwd is EXACTLY the same
/// main checkout — a session on a different path, or a non-Active one on the
/// same path, must not match. This is the pure core of the "two sessions on
/// one main checkout, no worktree isolating them" WARN path.
/// Test: itself.
#[test]
fn spawn_managed_on_main_warns_on_concurrent_main_checkout_session() {
    let checkout = std::path::Path::new("/Users/op/projects/writing");
    let other = std::path::Path::new("/Users/op/projects/other");

    // An Active session already rooted at the SAME checkout → detected.
    let mut same = stub_record(None, ManagedSessionState::Active, "tm-writing-01");
    same.cwd = checkout.to_path_buf();
    let found = has_concurrent_main_checkout_session(std::slice::from_ref(&same), checkout)
        .expect("an Active session sharing the exact cwd must be detected");
    assert_eq!(
        found.tmux_name, "tm-writing-01",
        "the detector must return the colliding record so the caller can name it"
    );

    // A DIFFERENT checkout path → no collision.
    assert!(
        has_concurrent_main_checkout_session(std::slice::from_ref(&same), other).is_none(),
        "a session on a different checkout must never collide"
    );

    // Same path but NOT Active (e.g. Stopped) → no collision.
    let mut stopped = stub_record(None, ManagedSessionState::Stopped, "tm-writing-02");
    stopped.cwd = checkout.to_path_buf();
    assert!(
        has_concurrent_main_checkout_session(std::slice::from_ref(&stopped), checkout).is_none(),
        "a non-Active session on the same checkout must not count as a live collision"
    );

    // Empty set → no collision.
    assert!(
        has_concurrent_main_checkout_session(&[], checkout).is_none(),
        "no sessions means no collision"
    );
}
