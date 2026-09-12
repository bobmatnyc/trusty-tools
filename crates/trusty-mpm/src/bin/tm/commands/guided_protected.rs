//! The guided fallback's protected-workspace launch step (#4300, #1724).
//!
//! Why: split out of `guided.rs` when #7603 pushed that file past the 500-SLOC
//! cap. The seam is a real one rather than an arbitrary cut — everything else in
//! `guided.rs` CLASSIFIES the current directory (is it a git tree, does it have
//! a GitHub remote, does this pane belong to a managed session), while this
//! function ACTS on the one classification that provisions: it composes
//! `managed_workspace` provisioning with `launch`, and it is the only place the
//! two meet.
//! What: [`launch_protected_workspace`], called from `guided::fallback_protected`
//! on its `OriginPlan::ManagedClone` arm.
//! Test: `guided_fallback_redirect_success_worktree_not_live_checkout`,
//! `guided_fallback_prepares_the_session_in_the_worktree_not_the_base_clone`,
//! `guided_fallback_leaves_no_tmux_session_behind` in `tests_behavior_b_tests.rs`.

/// Launch the guided-default fallback in the workspace this project is
/// entitled to — the protected managed clone, or its own main checkout.
///
/// Why: when the daemon is unreachable and the current directory is a GitHub-backed
/// git project, framework files must go into the managed-clone workspace
/// (`~/trusty-mpm-projects/<owner>/<repo>/.worktrees/<session-id>/`), never
/// into the operator's live checkout (#1724, #1803) — UNLESS the project is
/// registered with `worktree: false` (#3455), which is the operator saying
/// "run in my main checkout" and which the daemon honours via
/// `spawn_managed_on_main`. Before #4300 this path never consulted that
/// setting, so the opt-out silently held only while the daemon was up.
/// What: delegates the whole decision to
/// [`super::managed_workspace::provision_for_fallback`] — which reads the
/// registry BEFORE `ensure_base_clone`, so an opted-out project gets neither a
/// clone nor a worktree — then calls `launch()` against the resolved workspace.
/// On any failure (unparseable URL, clone error, worktree error) it returns
/// `Err` with an actionable message and the live checkout is never touched.
/// `gate` is the disk gate's measurement source (#7603): `MeasureTarget` in
/// production, pinned by the behaviour tests so they assert on placement rather
/// than on how full the runner's volume is.
/// Test: `guided_fallback_never_pollutes_github_git_checkout`,
/// `guided_fallback_redirect_success_worktree_not_live_checkout`; the #4300
/// opt-out cases live in `managed_workspace_tests.rs`.
pub(crate) async fn launch_protected_workspace(
    client: &reqwest::Client,
    url: &str,
    git_root: &std::path::Path,
    origin_url: &str,
    gate: &trusty_mpm::core::disk_usage_guard::DiskGate,
) -> anyhow::Result<()> {
    let session_id = trusty_mpm::session_manager::ManagedSessionId::new();
    // #7603: `gate` is `MeasureTarget` in production; a test pins it instead.
    let workspace = super::managed_workspace::provision_for_fallback(
        &trusty_mpm::project::registry_data_dir(),
        origin_url,
        git_root,
        &session_id,
        gate,
    )
    .await?;

    let dir = workspace.path().to_string_lossy().to_string();
    // #5274: the fallback already provisioned the protected worktree it
    // wants this session to run in, and passes it as `dir`; `launch` must not
    // provision a SECOND one on top, so the worktree request stays `false`.
    // #5836: it must not re-resolve that placement either — doing so redirected
    // the session into the shared base clone and abandoned this worktree.
    super::launch::launch(
        client,
        url,
        Some(dir),
        None,
        false,
        super::managed_workspace::LaunchDir::CallerResolved,
    )
    .await
}
