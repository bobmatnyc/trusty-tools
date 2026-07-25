//! Unit tests for [`super::ProjectRegistry`].
//!
//! Why: extracted from `registry.rs`'s inline `#[cfg(test)] mod tests` —
//! the #3822 `register_from_session` regression coverage pushed that file
//! over the 500-SLOC production cap (the whole file, prod + inline tests,
//! counts against the production cap since its basename doesn't match the
//! test-file naming convention). Following this crate's established
//! colocated-test-file pattern (e.g. `daemon/tmux.rs` + `daemon/tmux_tests.rs`,
//! `session_manager/manager.rs` + `session_manager/tests.rs`). Pure code
//! motion for the pre-existing tests — no behavior/assertion change.
//! What: register/get/list/update_with coverage, config seeding,
//! session-history auto-registration (`auto_register_from_sessions`), and
//! the #3822 spawn-time `register_from_session` create→list round-trip.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use super::*;
use crate::session_manager::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use chrono::Utc;
use std::path::PathBuf;
use tempfile::TempDir;

fn make_session_with_repo(repo_url: &str, branch: Option<&str>) -> SessionRecord {
    SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: "tmpm-test".into(),
        cwd: PathBuf::from("/tmp"),
        task: "test".into(),
        state: ManagedSessionState::Active,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path: None,
        repo_url: Some(repo_url.to_string()),
        branch: branch.map(String::from),
        pending_decision: None,
        proposed_default: None,
        correlation: Default::default(),
        runtime: Default::default(),
        ephemeral: false,
        workspace_owned: false,
        source_id: None,
        claude_session_id: None,
        scrollback_path: None,
        last_cwd: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: Default::default(),
        worktree_owner: None,
    }
}

fn make_session_no_repo() -> SessionRecord {
    SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: "tmpm-no-repo".into(),
        cwd: PathBuf::from("/tmp"),
        task: "no repo".into(),
        state: ManagedSessionState::Active,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path: None,
        repo_url: None,
        branch: None,
        pending_decision: None,
        proposed_default: None,
        correlation: Default::default(),
        runtime: Default::default(),
        ephemeral: false,
        workspace_owned: false,
        source_id: None,
        claude_session_id: None,
        scrollback_path: None,
        last_cwd: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: Default::default(),
        worktree_owner: None,
    }
}

#[tokio::test]
async fn registry_register_idempotent() {
    let dir = TempDir::new().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");

    let p = Project {
        name: "alpha".into(),
        repo_url: "https://github.com/o/alpha".into(),
        default_branch: "main".into(),
        stack_hint: None,
        tags: vec![],
        description: None,
        gh_user: None,
        gh_account: None,
        github: None,
        commit_name: None,
        commit_email: None,
        worktree: None,
    };
    registry.register(p.clone()).await.expect("register once");
    registry
        .register(p)
        .await
        .expect("register again (idempotent)");

    let all = registry.list().await.expect("list");
    assert_eq!(all.len(), 1, "idempotent registration must not duplicate");
}

#[tokio::test]
async fn registry_get() {
    let dir = TempDir::new().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");

    let p = Project {
        name: "beta".into(),
        repo_url: "https://github.com/o/beta".into(),
        default_branch: "develop".into(),
        stack_hint: Some("rust".into()),
        tags: vec!["backend".into()],
        description: Some("desc".into()),
        gh_user: Some("bob-work".into()),
        gh_account: None,
        github: None,
        commit_name: None,
        commit_email: None,
        worktree: None,
    };
    registry.register(p.clone()).await.expect("register");

    let got = registry.get("beta").await.expect("get");
    assert_eq!(got.name, "beta");
    assert_eq!(got.default_branch, "develop");
    assert_eq!(got.stack_hint.as_deref(), Some("rust"));
    assert_eq!(got.gh_user.as_deref(), Some("bob-work"));

    let err = registry.get("nonexistent").await;
    assert!(matches!(err, Err(ProjectStoreError::NotFound(_))));
}

fn make_project(name: &str) -> Project {
    Project {
        name: name.into(),
        repo_url: format!("https://github.com/o/{name}"),
        default_branch: "main".into(),
        stack_hint: None,
        tags: vec![],
        description: None,
        gh_user: None,
        gh_account: None,
        github: None,
        commit_name: None,
        commit_email: None,
        worktree: None,
    }
}

/// `update_with` propagates `NotFound` for an unregistered project and
/// never calls the mutation closure (there is nothing to mutate).
#[tokio::test]
async fn update_with_unknown_project_errors() {
    let dir = TempDir::new().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");
    let err = registry
        .update_with("nope", |p| p)
        .await
        .expect_err("unknown project must error");
    assert!(matches!(err, ProjectStoreError::NotFound(_)));
}

/// Reproduces, using the still-available low-level primitives (`get` +
/// `register`), the EXACT pre-fix PATCH hazard the #2481 review flagged as
/// a HIGH: a `get` under one lock followed LATER by a separate `register`
/// under a second lock leaves a window in which another task's own
/// get-then-register sequence can run to completion entirely — both tasks
/// build their full replacement record from the SAME stale snapshot, so
/// whichever `register` lands last silently discards the other's
/// already-"successful" field edit. This is the exact two-call shape the
/// OLD `patch_project_registry_route` used (fetch under one lock, mutate a
/// clone, then persist under a later, separate lock) — the shape
/// `update_with` replaces. A `tokio::sync::Notify` gate forces the
/// deterministic interleave (B's whole get-mutate-register sequence
/// completes while A is parked between its OWN get and its OWN register),
/// so the outcome never depends on scheduler luck.
/// Test: this IS the test (the #2481 HIGH regression guard, negative case).
#[tokio::test]
async fn get_then_register_pattern_reproduces_lost_update_pre_fix() {
    let dir = TempDir::new().expect("tempdir");
    let registry = Arc::new(ProjectRegistry::load(dir.path()).await.expect("load"));
    registry
        .register(make_project("widget"))
        .await
        .expect("seed");

    // Gate: task A fetches, then PARKS here — simulating the exact window
    // the two-lock pattern leaves open between its `get` and its
    // `register` — until B has fetched, mutated, AND registered its own
    // (different field) edit.
    let gate = Arc::new(tokio::sync::Notify::new());

    let reg_a = Arc::clone(&registry);
    let gate_a = Arc::clone(&gate);
    let a = tokio::spawn(async move {
        // Lock #1 (fetch), released as soon as `get` returns — exactly the
        // OLD handler's read.
        let mut current = reg_a.get("widget").await.unwrap();
        // Park until B's own get-mutate-register has fully landed.
        gate_a.notified().await;
        current.description = Some("set by A".into());
        // Lock #2 (register) — a SEPARATE, later acquisition than lock #1.
        reg_a.register(current).await.unwrap();
    });

    let reg_b = Arc::clone(&registry);
    let gate_b = Arc::clone(&gate);
    let b = tokio::spawn(async move {
        let mut current = reg_b.get("widget").await.unwrap();
        current.stack_hint = Some("rust".into());
        reg_b.register(current).await.unwrap();
        // B's committed write has landed — release A to clobber it.
        gate_b.notify_one();
    });

    b.await.unwrap();
    a.await.unwrap();

    // The bug: B's committed `stack_hint` edit is silently discarded — A's
    // later `register` persists a full record built from a snapshot taken
    // BEFORE B's write, so A's own field lands but B's is gone, with no
    // error ever surfaced to B's caller (B's own call returned Ok).
    let final_state = registry.get("widget").await.unwrap();
    assert_eq!(final_state.description.as_deref(), Some("set by A"));
    assert_eq!(
        final_state.stack_hint, None,
        "demonstrates the pre-fix hazard: B's committed stack_hint edit is \
         lost — silently clobbered by A's later register() against a \
         stale snapshot, even though B's own call reported success"
    );
}

/// Proves the fix: racing two `update_with` calls that edit DIFFERENT
/// fields of the SAME project can never lose either edit — both are
/// present in the final persisted record, regardless of which task's
/// write lands first. Pre-fix (the two-lock pattern exercised by
/// `get_then_register_pattern_reproduces_lost_update_pre_fix` above), this
/// same race would silently drop whichever edit lost the race.
/// Test: this IS the test (the #2481 HIGH regression guard, positive case).
#[tokio::test]
async fn update_with_serializes_concurrent_field_edits() {
    let dir = TempDir::new().expect("tempdir");
    let registry = Arc::new(ProjectRegistry::load(dir.path()).await.expect("load"));
    registry
        .register(make_project("widget"))
        .await
        .expect("seed");

    let reg_a = Arc::clone(&registry);
    let a = tokio::spawn(async move {
        reg_a
            .update_with("widget", |mut p| {
                p.description = Some("set by A".into());
                p
            })
            .await
    });

    let reg_b = Arc::clone(&registry);
    let b = tokio::spawn(async move {
        reg_b
            .update_with("widget", |mut p| {
                p.stack_hint = Some("rust".into());
                p
            })
            .await
    });

    a.await.unwrap().expect("A's update must succeed");
    b.await.unwrap().expect("B's update must succeed");

    // Both concurrent field edits must be present — neither was lost,
    // regardless of which task's write landed first.
    let final_state = registry.get("widget").await.unwrap();
    assert_eq!(final_state.description.as_deref(), Some("set by A"));
    assert_eq!(final_state.stack_hint.as_deref(), Some("rust"));
}

#[tokio::test]
async fn registry_list() {
    let dir = TempDir::new().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");

    for name in ["gamma", "delta", "epsilon"] {
        let p = Project {
            name: name.into(),
            repo_url: format!("https://github.com/o/{name}"),
            default_branch: "main".into(),
            stack_hint: None,
            tags: vec![],
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
            worktree: None,
        };
        registry.register(p).await.expect("register");
    }

    let all = registry.list().await.expect("list");
    assert_eq!(all.len(), 3);
}

#[tokio::test]
async fn registry_seed_from_config() {
    let dir = TempDir::new().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");

    let config_entries = vec![
        ProjectConfig {
            name: "from-config-a".into(),
            repo_url: "https://github.com/o/from-config-a".into(),
            default_branch: Some("main".into()),
            stack_hint: Some("python".into()),
            tags: Some(vec!["ml".into()]),
            description: Some("ml project".into()),
            gh_user: Some("bobmatnyc".into()),
            gh_account: Some("bobmatnyc".into()),
            github: Some(crate::core::trusty_tools_config::GithubConfig {
                config_dir: Some("/home/bob/.config/gh-ml".into()),
                token_env: None,
                account: None,
                host: None,
            }),
            commit_name: Some("ML Bot".into()),
            commit_email: Some("ml-bot@example.com".into()),
            untracked_sync: None,
            worktree: None,
        },
        ProjectConfig {
            name: "from-config-b".into(),
            repo_url: "https://github.com/o/from-config-b".into(),
            default_branch: None,
            stack_hint: None,
            tags: None,
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
            untracked_sync: None,
            worktree: None,
        },
    ];

    registry.seed_from_config(&config_entries).await;

    let all = registry.list().await.expect("list after seed");
    let names: Vec<&str> = all.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"from-config-a"), "{names:?}");
    assert!(names.contains(&"from-config-b"), "{names:?}");

    let a = registry.get("from-config-a").await.expect("get a");
    assert_eq!(a.stack_hint.as_deref(), Some("python"));
    assert_eq!(a.default_branch, "main");
    assert_eq!(a.gh_user.as_deref(), Some("bobmatnyc"));
    assert_eq!(a.gh_account.as_deref(), Some("bobmatnyc"));
    // #2184: the github/commit-identity binding must be mirrored onto the
    // registry record verbatim.
    assert_eq!(
        a.github.as_ref().and_then(|g| g.config_dir.as_deref()),
        Some(std::path::Path::new("/home/bob/.config/gh-ml"))
    );
    assert_eq!(a.commit_name.as_deref(), Some("ML Bot"));
    assert_eq!(a.commit_email.as_deref(), Some("ml-bot@example.com"));

    // Entry without default_branch must fall back to "main"; gh_user and
    // the #2184 github/commit-identity fields stay unset (no regression
    // for configs that predate #2081/#2184).
    let b = registry.get("from-config-b").await.expect("get b");
    assert_eq!(b.default_branch, "main");
    assert_eq!(b.gh_user, None);
    assert_eq!(b.gh_account, None);
    assert_eq!(b.github, None);
    assert_eq!(b.commit_name, None);
    assert_eq!(b.commit_email, None);
}

#[tokio::test]
async fn registry_auto_register_from_sessions() {
    let dir = TempDir::new().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");

    let sessions = vec![
        make_session_with_repo("https://github.com/owner/trusty-tools.git", Some("main")),
        make_session_with_repo("https://github.com/owner/another-repo", None),
        make_session_no_repo(), // must be silently skipped
    ];
    registry.auto_register_from_sessions(&sessions).await;

    let all = registry.list().await.expect("list after auto-register");
    let names: Vec<&str> = all.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"trusty-tools"), "{names:?}");
    assert!(names.contains(&"another-repo"), "{names:?}");
    assert_eq!(all.len(), 2, "no-repo session must be skipped");

    let p = registry
        .get("trusty-tools")
        .await
        .expect("get trusty-tools");
    assert_eq!(p.default_branch, "main");
    assert_eq!(p.repo_url, "https://github.com/owner/trusty-tools.git");

    let p2 = registry
        .get("another-repo")
        .await
        .expect("get another-repo");
    assert_eq!(p2.default_branch, "main", "no branch falls back to main");
}

// ── #3822: spawn-time `register_from_session` (create→list round-trip) ──

/// The core #3822 regression: a session created AFTER the registry's
/// one-shot `auto_register_from_sessions` boot pass has already run
/// (simulated here by simply never calling it) must still become
/// visible to `list()` once `register_from_session` is called — the
/// explicit spawn-time counterpart the lifecycle spawn paths now call.
#[tokio::test]
async fn registry_register_from_session_registers_new_project() {
    let dir = TempDir::new().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");

    // Registry starts empty — as it would for a daemon whose boot
    // auto-register pass saw zero sessions (the #3822 repro: `tm start`
    // touches the registry before any session exists).
    assert!(registry.list().await.expect("list").is_empty());

    let session =
        make_session_with_repo("https://github.com/octocat/Hello-World.git", Some("master"));
    registry.register_from_session(&session).await;

    let all = registry
        .list()
        .await
        .expect("list after register_from_session");
    assert_eq!(
        all.len(),
        1,
        "the session's project must be visible immediately after create — no dependency \
         on which caller happened to touch the registry first"
    );
    let p = registry.get("Hello-World").await.expect("get Hello-World");
    assert_eq!(p.repo_url, "https://github.com/octocat/Hello-World.git");
    assert_eq!(p.default_branch, "master");
}

/// A session with no `repo_url` (e.g. a bare local-path spawn with no
/// parseable remote) must be a silent no-op — never an error, never a
/// spurious registration.
#[tokio::test]
async fn registry_register_from_session_skips_when_repo_url_missing() {
    let dir = TempDir::new().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");

    registry
        .register_from_session(&make_session_no_repo())
        .await;

    assert!(
        registry.list().await.expect("list").is_empty(),
        "a repo_url-less session must never produce a project entry"
    );
}

/// `register_from_session` must never clobber an existing entry —
/// mirrors `auto_register_from_sessions`'s own "skip if already
/// registered" guarantee, so a manually-configured project's fields
/// (stack_hint, tags, gh identity, …) survive a later session spawn for
/// the same repo.
#[tokio::test]
async fn registry_register_from_session_does_not_overwrite_existing() {
    let dir = TempDir::new().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");

    let manual = Project {
        name: "Hello-World".into(),
        repo_url: "https://github.com/octocat/Hello-World.git".into(),
        default_branch: "main".into(),
        stack_hint: Some("rust".into()),
        tags: vec!["manual".into()],
        description: Some("hand-configured".into()),
        gh_user: None,
        gh_account: None,
        github: None,
        commit_name: None,
        commit_email: None,
        worktree: None,
    };
    registry
        .register(manual.clone())
        .await
        .expect("manual register");

    let session =
        make_session_with_repo("https://github.com/octocat/Hello-World.git", Some("master"));
    registry.register_from_session(&session).await;

    let all = registry.list().await.expect("list");
    assert_eq!(all.len(), 1, "must not duplicate");
    let p = registry.get("Hello-World").await.expect("get");
    assert_eq!(
        p.stack_hint,
        Some("rust".into()),
        "the manually-configured entry's fields must survive untouched"
    );
    assert_eq!(
        p.default_branch, "main",
        "must not be overwritten by the session's branch"
    );
}

#[tokio::test]
async fn registry_auto_register_skips_already_registered() {
    let dir = TempDir::new().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");

    // Pre-register with explicit description.
    let existing = Project {
        name: "trusty-tools".into(),
        repo_url: "https://github.com/owner/trusty-tools".into(),
        default_branch: "develop".into(),
        stack_hint: Some("rust".into()),
        tags: vec![],
        description: Some("manually registered".into()),
        gh_user: None,
        gh_account: None,
        github: None,
        commit_name: None,
        commit_email: None,
        worktree: None,
    };
    registry.register(existing).await.expect("pre-register");

    // Auto-register with a different URL for the same name — must not overwrite.
    let sessions = vec![make_session_with_repo(
        "https://github.com/owner/trusty-tools.git",
        Some("feature"),
    )];
    registry.auto_register_from_sessions(&sessions).await;

    let p = registry.get("trusty-tools").await.expect("get");
    assert_eq!(
        p.description.as_deref(),
        Some("manually registered"),
        "auto-register must not overwrite existing entry"
    );
    assert_eq!(p.default_branch, "develop");
}
