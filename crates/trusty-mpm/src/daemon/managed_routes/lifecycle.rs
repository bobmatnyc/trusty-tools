//! Transport-agnostic session-lifecycle core shared by the HTTP routes and the
//! MCP tools (#1221).
//!
//! Why: both the HTTP `…/managed/*` handlers (in this module's `mod.rs`) and the
//! MCP session-lifecycle tools (in `crate::daemon::mcp_session`) must spawn and
//! resume managed sessions with IDENTICAL semantics. Keeping the spawn/resume
//! flow here — rather than duplicating it per transport — guarantees they cannot
//! diverge, and keeps the route file under the 500-SLOC production cap.
//! What: the `SpawnParams` input struct, the `spawn_managed` flow
//! (provision → create tmux host → launch harness), the typed `ResumeManagedError`
//! (so callers map failures to HTTP 404/409/500 by VARIANT, never by `Display`
//! substring), and the `resume_managed` flow (single-round-trip resume + respawn,
//! no TOCTOU pre-flight `get`).
//! Test: `spawn_managed`/`resume_managed` are exercised by the HTTP spawn/resume
//! handler tests and the MCP `session_new_invalid_runtime_errors` /
//! `session_resume_unknown_id_errors` tests, plus the typed-error regression
//! tests `resume_managed_typed_*` in tests/session_manager_mvp.rs.

use std::sync::Arc;

use tracing::{info, warn};

use super::inproject::try_inproject_spawn;
use crate::daemon::state::DaemonState;
use crate::provisioner::WorkspaceProvisioner;
use crate::runtime::{RuntimeKind, build_adapter};
use crate::session_manager::{ManagedError, ManagedSessionId, ManagedSessionState, SessionRecord};

/// Transport-agnostic inputs for spawning a managed session.
///
/// Why: both the HTTP `POST /…/managed` handler and the MCP `session_new` tool
/// need to spawn a session with the same semantics; a shared struct lets one
/// [`spawn_managed`] function serve both without the MCP path re-implementing
/// the provision→create→spawn ritual.
/// What: the same fields as `super::SpawnRequest` but plain owned types (no
/// axum/serde extraction), so non-HTTP callers can build it directly.
/// Test: `spawn_managed` is exercised via `crate::daemon::mcp_session`'s
/// `session_new_invalid_runtime_errors` and the HTTP spawn tests.
#[derive(Debug, Clone)]
pub struct SpawnParams {
    /// Repository URL to provision the session workspace from.
    pub repo_url: String,
    /// Git branch or ref to check out.
    pub git_ref: String,
    /// Human-readable task description for the session.
    pub task: String,
    /// Optional name hint overriding the auto-generated tmux session name.
    pub name_hint: Option<String>,
    /// Optional runtime selector (`"claude-code"` | `"tcode"`).
    pub runtime: Option<String>,
    /// Whether the spawned session is EPHEMERAL (a test/throwaway session) (#1508).
    ///
    /// Why: e2e harnesses (and any caller that knows it is creating a disposable
    /// session) set this so the bulk-teardown and age-based reap paths may clean
    /// the session up automatically. `None`/`Some(false)` → a normal, durable
    /// session that the automatic paths never touch.
    pub ephemeral: Option<bool>,
    /// Whether this spawn originates from the MCP tool surface (#1836, #1837).
    ///
    /// Why: an MCP-triggered `session_new` call can mint real infrastructure
    /// for any repo an LLM caller names, with zero operator confirmation (the
    /// ARIA incident). `true` subjects the spawn to the two-layer MCP spawn
    /// gate (off-by-default + registry allowlist,
    /// [`super::mcp_spawn_gate::ensure_mcp_spawn_allowed`]) BEFORE any
    /// provisioning begins. `false` — set by the HTTP route (`tm launch`/`tm
    /// ticket` clients) and the SM-STDIO adapter (`sm.sessions.launch`) — never
    /// gates; those are trusted, explicitly-operator-driven paths.
    pub mcp_initiated: bool,
    /// Whether to AUTO-INJECT `task` into the spawned pane once the runtime is
    /// ready (issues #1903 / #1299).
    ///
    /// Why: `--task` is meant to be turnkey — the session should start working
    /// on the task immediately after spawn. `None`/`Some(true)` inject (the
    /// default, so `tm session new`, `tm ticket`, and MCP/SM callers are all
    /// turnkey without opting in). `Some(false)` selects the legacy
    /// metadata-only behavior (`tm session new --no-inject`), for callers that
    /// deliver the task themselves via `tm session send`. Injection is
    /// additionally gated by [`crate::session_manager::should_inject_task`]
    /// (non-empty task, Claude Code runtime, session reached `Active`).
    pub inject_task: Option<bool>,
    /// Optional Deliverable id to bind this session to (DOC-35 §10.6, #2379).
    ///
    /// Why: `tm sessions new --deliverable <id>` links a session to a
    /// Deliverable so its status/ls output can surface which unit of work the
    /// session is advancing (§10.6: 1 Deliverable ↔ many Sessions). Carried as
    /// the raw stringified id here — the HTTP route (`spawn_session`) is
    /// responsible for validating the Deliverable exists AND belongs to the
    /// spawning project BEFORE any provisioning side effect (a 404-style
    /// [`crate::daemon::error::DaemonError::DeliverableNotFound`] otherwise);
    /// [`spawn_managed`] trusts that pre-validation and just persists the
    /// pointer post-creation via
    /// [`crate::session_manager::SessionManager::set_deliverable_id`]. A
    /// malformed or absent id is silently a no-op link (`None`) — never a
    /// reason to fail an otherwise-successful spawn.
    pub deliverable_id: Option<String>,
    /// Whether to FORCE a brand-new session even when a live in-project session
    /// for the same project already exists (#2450).
    ///
    /// Why: the #1707 in-project reconnect pre-flight silently ADOPTS an
    /// existing live session for the same `source_id` (returning it instead of
    /// spawning). A surface that explicitly means "launch a NEW session" — the
    /// `tm` picker's "[N] launch new session" choice, `tm session new`/`session
    /// start` — would otherwise have its `--task` injected into an unrelated
    /// live session. `true` SKIPS the reconnect pre-flight (always spawns
    /// fresh); `false` (the default, so programmatic/idempotent callers — MCP
    /// `session_new`, the SM-STDIO adapter, the chat surfaces — keep
    /// reconnecting) preserves #1707.
    pub force_new: bool,
}

/// Spawn a managed session, shared by the HTTP handler and the MCP tool.
///
/// Why: the spawn flow (resolve runtime → provision workspace → create tmux host
/// → launch harness) must be identical across transports; centralising it here
/// means the MCP `session_new` tool is a true thin wrapper rather than a
/// divergent copy.
/// What: in order — (0) parses the runtime selector (an unknown value is an early
/// `Err` before any side effect); (1) provisions an isolated workspace; (2)
/// creates the tmux session rooted at that workspace; (3) spawns the selected
/// runtime in the pane (a spawn failure marks the record errored but is not
/// fatal — the record still exists). Returns the final [`SessionRecord`].
/// Test: `crate::daemon::mcp_session::tests::session_new_invalid_runtime_errors`
/// covers the early runtime-rejection path; the HTTP spawn tests cover the
/// provision/create/spawn path.
pub async fn spawn_managed(
    state: &Arc<DaemonState>,
    params: SpawnParams,
) -> Result<SessionRecord, String> {
    // Config is loaded ONCE here and reused below for both the MCP spawn gate
    // and the workspace-root resolution, so the two cannot read divergent
    // snapshots of `config.yaml` within a single spawn.
    let config = crate::core::trusty_tools_config::TrustyToolsConfig::load();

    // Step 0: resolve the runtime backend (default claude-code). Reject unknown
    // selectors BEFORE any provisioning so a typo never leaves an orphan
    // workspace.
    let runtime = match params.runtime.as_deref() {
        None => RuntimeKind::default(),
        Some(raw) => raw.parse::<RuntimeKind>().map_err(|e| e.to_string())?,
    };

    // Step 0.4 — MCP SPAWN GATE (#1836, #1837): an MCP-triggered spawn (never a
    // `tm launch`/`tm ticket`/SM-STDIO call — see `SpawnParams::mcp_initiated`)
    // must be refused BEFORE any side effect when MCP spawning is disabled
    // (the default) or the target repo is not an already-known project. This
    // runs before `ManagedSessionId::new()` so a refusal mints nothing.
    if params.mcp_initiated {
        let registry = state.project_registry().await;
        super::mcp_spawn_gate::ensure_mcp_spawn_allowed(&registry, &config, &params.repo_url)
            .await?;
    }

    let session_id = ManagedSessionId::new();

    // Wrap the ENTIRE spawn dispatch — in-project, local-path, AND clone-based
    // — in a `provisioning_stage` scope (issue #1904 stretch goal; #1919 fix).
    // Before #1919 only the clone-based tail (`spawn_managed_cloned`) was
    // wrapped here, so the `is_local_workdir` branch below — covering BOTH
    // `spawn_managed_inproject` and `spawn_managed_local` — returned before
    // this scope was ever installed. Since #1916 unified `tm session start`
    // onto `spawn_managed_inproject`, that branch is now the dominant path,
    // so every `emit(...)` call anywhere in ITS call tree (including
    // `try_inproject_spawn`'s `ensure_base_clone` first-run clone, several
    // layers deep in `inproject.rs`) now also broadcasts on the daemon's
    // existing SSE channel — no signature changes needed anywhere in that
    // call tree, per `core::provisioning_stage`'s module doc. The client
    // correlates by `repo_url` (it cannot know `session_id` until this whole
    // call returns).
    let emitter = crate::core::provisioning_stage::StageEmitter::new(
        session_id.to_string(),
        params.repo_url.clone(),
        state.event_tx.clone(),
    );
    // Capture the injection opt-out and the (already HTTP-layer-validated)
    // Deliverable link before `params` is moved into the routed dispatch.
    // inject_task: `None`/`Some(true)` → turnkey (inject); `Some(false)` →
    // metadata-only (`--no-inject`).
    let inject_flag = params.inject_task != Some(false);
    let deliverable_id = params.deliverable_id.clone();
    let mut record = crate::core::provisioning_stage::scoped(
        emitter,
        spawn_managed_routed(state, session_id, params, runtime, config),
    )
    .await?;

    // Deliverable linkage (DOC-35 §10.6, #2379): the HTTP route already
    // validated `deliverable_id` exists and belongs to this project BEFORE
    // provisioning started, so this is a trusting, non-fatal persist of a
    // PURE POINTER — never a reason to fail an otherwise-successful spawn,
    // and never a mutation of the Deliverable itself (§11 forbids
    // auto-transitions). The local `record` is updated too so the immediate
    // spawn response already reflects the link, not just later `GET`s.
    if let Some(raw_id) = deliverable_id {
        match raw_id.parse::<crate::deliverable::DeliverableId>() {
            Ok(did) => match state
                .session_manager()
                .await
                .set_deliverable_id(&record.id, did)
                .await
            {
                Ok(()) => record.deliverable_id = Some(did),
                Err(e) => warn!(
                    id = %record.id,
                    "spawn_managed: set_deliverable_id failed: {e}; deliverable link not recorded"
                ),
            },
            Err(e) => warn!(
                id = %record.id,
                "spawn_managed: deliverable_id {raw_id:?} failed to re-parse (already validated by the HTTP layer): {e}"
            ),
        }
    }

    // Turnkey task injection (#1903 / #1299): once the pane is up and the
    // runtime is ready, deliver the task through the same seam `tm session send`
    // uses, so `tm session new --task` / `tm ticket` start working immediately
    // instead of sitting idle at an empty prompt.
    spawn_task_injection(state.clone(), record.clone(), inject_flag);

    Ok(record)
}

/// Kick off turnkey `--task` injection for a freshly-spawned session (#1903 / #1299).
///
/// Why: `--task` must be turnkey — after the runtime launches, the task is typed
/// into the pane so the session starts working immediately. Delivery has to WAIT
/// for the runtime to be ready (keystrokes sent before `claude` execs are lost),
/// which can take a few seconds; blocking the spawn response on that would make
/// every caller wait, so this runs in a detached background task (mirroring
/// `daemon::services::session_service::spawn_pid_capture`).
/// What: gates on [`crate::session_manager::should_inject_task`] (opt-out flag,
/// non-empty task, Claude Code runtime — `tcode` injects the task in its own
/// launch command — and a session that reached `Active`); when it passes, spawns
/// a Tokio task that awaits
/// [`crate::session_manager::SessionManager::inject_task_when_ready`]. A skip or
/// failure is non-fatal: the task remains on the record as metadata, deliverable
/// via `tm session send`.
/// Test: the gate is unit-tested in `session_manager::task_inject`; the delivery
/// via the fake seam is covered there too. This wiring is side-effect-only
/// (fire-and-forget) and exercised by the live `#[ignore]` e2e flow.
fn spawn_task_injection(state: Arc<DaemonState>, record: SessionRecord, inject_flag: bool) {
    if !crate::session_manager::should_inject_task(
        inject_flag,
        &record.task,
        record.runtime,
        &record.state,
    ) {
        return;
    }
    tokio::spawn(async move {
        let mgr = state.session_manager().await;
        match mgr.inject_task_when_ready(&record.id, &record.task).await {
            Ok(true) => {}
            Ok(false) => warn!(
                id = %record.id,
                name = %record.tmux_name,
                "turnkey task injection skipped (runtime never became ready); task retained as metadata"
            ),
            Err(e) => warn!(
                id = %record.id,
                name = %record.tmux_name,
                "turnkey task injection failed: {e}; task retained as metadata"
            ),
        }
    });
}

/// Route a spawn to the in-project, local-path, or clone-based branch (#1919).
///
/// Why: extracted from [`spawn_managed`] so the `provisioning_stage::scoped`
/// wrapper installed there covers ALL THREE spawn branches uniformly — this
/// function is exactly the routing logic `spawn_managed` used to run inline
/// after `is_local_workdir` detection, before #1919 moved the scope to cover it.
/// What: local-path fast path (#1433) — if `repo_url` is an existing local
/// absolute directory, tries in-project DETECTION (#1706, via
/// `try_inproject_spawn`, which since #2032 only ensures the base clone and
/// returns `(base_path, owner, repo)` — it no longer creates the worktree
/// itself). On a match, this function resolves the SEMANTIC tmux name here —
/// BEFORE the per-session worktree exists — via
/// `SessionManager::resolve_session_name`, folding in
/// `inproject::worktree_name_collides` as the extra collision predicate so a
/// name whose worktree dir/branch already exists is retried with the next
/// free serial instead of silently colliding. It then creates the worktree at
/// that resolved name (`inproject::create_session_worktree`) and dispatches to
/// `spawn_managed_inproject`, threading the resolved name through so
/// `create_with_id` never re-derives it (issue #2032 — a session's tmux name
/// is derived in exactly ONE place). Any failure in detection, name
/// resolution, or worktree creation falls through to `spawn_managed_local`. A
/// remote repo URL (the common case) falls straight through to
/// `spawn_managed_cloned`.
/// Test: exercised transitively by the same tests that covered the inline
/// version before extraction (HTTP spawn tests, MCP session tests); the
/// stage-emission behaviour added by #1919 is covered by
/// `prepare_inproject_session_emits_stage_events_in_order` in this module's
/// `tests` submodule and `ensure_base_clone_emits_cloning_repo_only_on_fresh_clone`
/// in `inproject::tests`.
async fn spawn_managed_routed(
    state: &Arc<DaemonState>,
    session_id: ManagedSessionId,
    params: SpawnParams,
    runtime: RuntimeKind,
    config: crate::core::trusty_tools_config::TrustyToolsConfig,
) -> Result<SessionRecord, String> {
    // Detection heuristic (documented): the string is an ABSOLUTE path that EXISTS
    // and is a DIRECTORY on the daemon host. Anything else (a `https://…` /
    // `git@…` URL, a relative path, a non-existent path) falls through to the
    // existing clone-based provisioning below, so remote-URL callers are unaffected.
    if is_local_workdir(&params.repo_url) {
        // In-project path (#1706): if the local directory is a git repo with a
        // GitHub remote, spawn against a per-session worktree of a protected base
        // clone rather than using the directory directly. If it is NOT a git repo
        // with a GitHub remote (no `.git`, no remote origin, non-GitHub URL), fall
        // through to the existing local-path spawn.
        let local_path = std::path::Path::new(&params.repo_url);
        match try_inproject_spawn(local_path) {
            Ok(Some((base, owner, repo))) => {
                // Reconnect pre-flight (#1707), HOISTED AHEAD of worktree
                // reservation (#2450): the reconnect used to live inside
                // `spawn_managed_inproject`, which meant `reserve_inproject_worktree`
                // had already created (then left for pruning) a fresh
                // `.worktrees/<name>` slice before the reconnect returned the
                // existing session — worktree litter, including a locked orphan.
                // Running the check here, before any worktree is reserved, means
                // a reconnect creates ZERO worktrees. Gated by `force_new` so an
                // explicit "launch new" surface always spawns fresh instead of
                // adopting an unrelated live session for the same project.
                {
                    let mgr = state.session_manager().await;
                    let existing = mgr.list().await;
                    let source_id = format!("{owner}/{repo}");
                    if let Some(live) = reconnect_candidate(
                        params.force_new,
                        &existing,
                        &source_id,
                        &*mgr.tmux_driver(),
                    ) {
                        info!(
                            id = %live.id,
                            source_id = %source_id,
                            "spawn_managed (inproject): reconnecting to existing live session"
                        );
                        return Ok(live);
                    }
                }
                match reserve_inproject_worktree(
                    state,
                    &session_id,
                    &params,
                    local_path,
                    &base,
                    &repo,
                    &config,
                )
                .await
                {
                    Ok((worktree, reserved_name)) => {
                        return spawn_managed_inproject(
                            state,
                            &session_id,
                            &params,
                            runtime,
                            worktree,
                            owner,
                            repo,
                            reserved_name,
                        )
                        .await;
                    }
                    Err(e) => {
                        tracing::warn!(
                            id = %session_id,
                            "in-project spawn: {e}; falling back to local-path spawn"
                        );
                    }
                }
            }
            Ok(None) => {
                // Not a git repo with a GitHub remote — use local-path fast path.
            }
            Err(e) => {
                tracing::warn!(
                    id = %session_id,
                    "in-project spawn failed: {e}; falling back to local-path spawn"
                );
            }
        }
        return spawn_managed_local(state, &session_id, &params, runtime).await;
    }

    spawn_managed_cloned(state, session_id, params, runtime, config).await
}

/// Resolve the semantic tmux name and create the per-session worktree for the
/// in-project spawn path (issue #2032).
///
/// Why: split out of [`spawn_managed_routed`] so the name-resolution-then-
/// worktree-creation sequence — the actual #2032 hoist — reads as one clear
/// step, and so its two fallible sub-steps (name resolution, worktree
/// creation) share one error-formatting path back to the caller's
/// fall-through-to-local-path handling.
/// What: (1) resolves a collision-free tmux name via
/// `SessionManager::resolve_session_name`, using `params.name_hint`, the
/// detected repo name as the GitHub-project fallback (`cwd` is passed only as
/// the third-priority basename fallback — it is never actually reached here
/// since the repo name is always `Some`), and
/// `inproject::worktree_name_collides(base, candidate)` as the extra
/// collision predicate (so a name whose `.worktrees/<name>` dir or
/// `session/<name>` branch already exists is retried with the next serial,
/// not silently reused); (2) creates the worktree at that name via
/// `inproject::create_session_worktree`; (3) best-effort syncs the
/// operator's allowlisted untracked/secret files (`.env*` by default, #2196)
/// from `local_path` (the operator's live checkout) into the new worktree via
/// [`super::inproject::untracked_sync::sync_untracked_files`] — resolved
/// per-project-over-global-over-default via
/// `trusty_tools_config::resolve_untracked_sync(config, Some(repo))`, and
/// SKIPPED entirely (not even attempted) when resolution says `enabled ==
/// false`. Step 3 can never fail this function: sync failures are `warn!`-
/// logged inside `sync_untracked_files` itself and never propagate. Returns
/// `(worktree_path, reserved_name)` so the caller can create the tmux
/// session under the SAME name without re-deriving it.
/// Test: exercised transitively by the in-project spawn integration tests;
/// the sync step's own behaviour (matching, size cap, path-escape guard,
/// `.git/info/exclude` append) is unit-tested directly in
/// `inproject::untracked_sync::tests`.
async fn reserve_inproject_worktree(
    state: &Arc<DaemonState>,
    session_id: &ManagedSessionId,
    params: &SpawnParams,
    local_path: &std::path::Path,
    base: &std::path::Path,
    repo: &str,
    config: &crate::core::trusty_tools_config::TrustyToolsConfig,
) -> Result<(std::path::PathBuf, String), String> {
    let mgr = state.session_manager().await;
    let reserved_name = mgr
        .resolve_session_name(
            params.name_hint.as_deref(),
            Some(repo),
            local_path,
            |candidate| super::inproject::worktree_name_collides(base, candidate),
        )
        .await
        .map_err(|e| format!("name resolution failed for session {session_id}: {e}"))?;

    let worktree = super::inproject::create_session_worktree(base, &reserved_name)
        .map_err(|e| format!("worktree creation failed for session {session_id}: {e}"))?;

    // #2196: best-effort sync of the operator's allowlisted untracked/secret
    // files (default `.env*`) from the live checkout into the fresh
    // worktree. Never fails the spawn — see the doc above.
    let resolved_sync =
        crate::core::trusty_tools_config::resolve_untracked_sync(config, Some(repo));
    if resolved_sync.enabled {
        super::inproject::untracked_sync::sync_untracked_files(
            local_path,
            &worktree,
            &resolved_sync.patterns,
        );
    }

    Ok((worktree, reserved_name))
}

/// The clone-based provisioning tail of [`spawn_managed_routed`] (issue #1904).
///
/// Why: split out so the (now-shared, #1919) stage-observer scope installed
/// in `spawn_managed` has a clean per-branch extraction to call — mirroring
/// `spawn_managed_inproject`/`spawn_managed_local`, which get the identical
/// scope for free since #1919 moved the `scoped(...)` wrapper up to cover all
/// three branches uniformly (before #1919 this was the ONLY branch it wrapped).
/// What: provisions the workspace (emits `CloningRepo`/`DeployingAgents`/
/// `DeployingSkills`/`BuildingInstructions`/`ConfiguringMcp` from deep inside
/// `provision`/`provision_in`/`prepare_session_inner`), creates the tmux
/// session (`CreatingTmuxSession`), runs the FRONT gate, spawns the runtime
/// (`LaunchingRuntime`), and emits `Complete`. Identical behaviour to the
/// pre-#1904 inline tail of `spawn_managed` — this is a pure extraction plus
/// `emit(...)` calls.
/// Test: `handler_spawn_wires_provision_and_spawn` /
/// `handler_spawn_creates_tmux_at_workspace_cwd` in tests/session_manager_mvp.rs
/// cover the behaviour; the stage-emission itself is unit-tested in
/// `core::provisioning_stage` and `provisioner::workspace`/`session_launch`
/// (no live daemon needed for the emission tests).
async fn spawn_managed_cloned(
    state: &Arc<DaemonState>,
    session_id: ManagedSessionId,
    params: SpawnParams,
    runtime: RuntimeKind,
    config: crate::core::trusty_tools_config::TrustyToolsConfig,
) -> Result<SessionRecord, String> {
    use crate::core::provisioning_stage::{ProvisioningStage, emit};

    // #2184: resolve the per-project gh identity + commit identity ONCE for
    // this spawn (project `github:`/commit fields in `config.projects` >
    // global `github:` > fully ambient) and apply it to every git subprocess
    // the provisioner runs below. A `GhIdentityError` (the `account`-only
    // refusal — see `core::gh_identity` module docs) is surfaced as a spawn
    // failure BEFORE any provisioning side effect, mirroring how the MCP
    // spawn gate above already fails closed before touching disk.
    let git_identity = crate::core::git_identity::resolve_for_config(&config, &params.repo_url)
        .map_err(|e| {
            warn!(id = %session_id, "spawn_managed: git identity resolution failed: {e}");
            format!("git identity resolution failed: {e}")
        })?;

    // Provision an isolated workspace under the pre-generated `session_id` (the id
    // is generated ONCE in `spawn_managed`, before the local-path/clone branch
    // split, so both branches register the same id).
    //
    // #1220: the workspace root defaults to `~/trusty-mpm-projects/` (overridable
    // via the `TRUSTY_MPM_WORKSPACE_ROOT` env var or the
    // `~/.trusty-tools/trusty-mpm/config.yaml` `workspace_root_template`), and the
    // session nests under the target repo's GitHub `<owner>/<repo>` identity:
    // `<root>/<owner>/<repo>/<session-id>/`. When the repo URL has no parseable
    // GitHub identity we fall back to the legacy single-slug `provision` path so a
    // bare/non-GitHub URL still provisions cleanly. `config` was already loaded
    // in `spawn_managed` (before the MCP spawn gate); reused here for the
    // workspace root. `CloningRepo`/`DeployingAgents`/`DeployingSkills`/
    // `BuildingInstructions`/`ConfiguringMcp` are emitted from inside `provision`/
    // `provision_in`/`prepare_session_inner` — not here — since those are the
    // functions that actually perform each step.
    let prepared = match trusty_common::github_path::parse_github_path(&params.repo_url) {
        Some(gh) => {
            let project_dir = crate::core::trusty_tools_config::workspace_subpath(&config, &gh);
            // `provision_in` only appends the session id; pass an empty workspace
            // root because the project dir is already absolute.
            let provisioner = WorkspaceProvisioner::new(
                crate::provisioner::RealGitBackend::new(git_identity),
                std::path::PathBuf::new(),
            );
            provisioner.provision_in(
                &project_dir,
                &session_id,
                &params.repo_url,
                &params.git_ref,
                &params.task,
            )
        }
        None => {
            let workspace_root = crate::core::trusty_tools_config::workspace_root(&config);
            let provisioner = WorkspaceProvisioner::new(
                crate::provisioner::RealGitBackend::new(git_identity),
                workspace_root,
            );
            provisioner.provision(&session_id, &params.repo_url, &params.git_ref, &params.task)
        }
    }
    .map_err(|e| {
        warn!(id = %session_id, "spawn_managed: provision failed: {e}");
        format!("workspace provisioning failed: {e}")
    })?;

    // Step 2: create the tmux session rooted at the provisioned workspace.
    // #1935: `owned=false` — the workspace is now a `git worktree` slice of a
    // shared, persistent base checkout (`<project_dir>/.base/`), not an
    // independently-owned full clone. Bulk `remove_dir_all` would leave the
    // base checkout's git worktree metadata and session branch ref dangling;
    // `session_manager::decommission` instead detects the `.worktrees/<id>`
    // shape (`is_session_worktree`) and runs `git worktree remove --force` +
    // branch cleanup via `remove_session_worktree`, mirroring exactly how the
    // in-project spawn path (`spawn_managed_inproject`, below) already handles
    // its own per-session worktrees.
    emit(ProvisioningStage::CreatingTmuxSession);
    let mgr = state.session_manager().await;
    let record = mgr
        .create_with_id(
            session_id,
            params.task.clone(),
            Some(prepared.path.clone()),
            params.name_hint,
            Some(prepared.path.clone()),
            Some(params.repo_url.clone()),
            Some(params.git_ref.clone()),
            runtime,
            params.ephemeral.unwrap_or(false),
            false, // owned: false — worktree of a shared base checkout, not a full clone
        )
        .await
        .map_err(|e| {
            warn!(id = %session_id, "spawn_managed: session create failed: {e}");
            e.to_string()
        })?;

    // Step 2.5 — INTENT-CONFORMANCE FRONT GATE (#1360, spec §5.1).
    //
    // Between record-creation and `adapter.spawn`, resolve the ticket+spec intent
    // for this task and decide whether to auto-proceed or escalate BEFORE any code
    // is written. The gate is fail-open (non-ticketed work, an unresolved ISR, or
    // a gap all auto-proceed) so it can never be the reason a spawn stalls. On an
    // escalation it withholds the spawn, writes `pending_decision`/`proposed_default`
    // (surfaced through every existing channel), and leaves the session in its
    // pre-spawn `Provisioning` state until a human resolves it via `POST …/answer`.
    if let Some(record) =
        front_gate_or_escalate(&mgr, &record, &params.repo_url, &params.task).await?
    {
        return Ok(record);
    }

    if let Err(e) = mgr
        .set_workspace(
            &record.id,
            prepared.path.clone(),
            ManagedSessionState::Active,
        )
        .await
    {
        warn!(id = %record.id, "spawn_managed: set_workspace failed: {e}");
    }

    // Deployment-completeness check (#2158, made non-blocking by #2172): best-
    // effort auto-repair of an incomplete `.claude/` payload. #2171 tracks the
    // validator over-reporting INCOMPLETE; until that lands, a false positive
    // here must never withhold the runtime launch (P0: it was leaving every
    // session at a bare shell). Any gap — real or falsely reported — is now
    // logged and the session proceeds to `adapter.spawn` regardless.
    let fw = crate::core::paths::FrameworkPaths::for_managed_workspace(&prepared.path);
    if let Err(reason) =
        ensure_deployment_complete(&fw, &prepared.path, record.repo_url.as_deref(), &record.id)
    {
        warn!(id = %record.id, "spawn_managed: deployment incomplete after auto-repair (non-blocking, launch proceeds): {reason}");
    }

    // Step 3: spawn the selected runtime in the pane. A spawn failure is recorded
    // (the record is marked errored) but is not fatal — the record exists and the
    // caller still gets it back.
    emit(ProvisioningStage::LaunchingRuntime);
    let tmux_arc = mgr.tmux_driver();
    let adapter = build_adapter(record.runtime, tmux_arc);
    if let Err(e) = adapter.spawn(
        &record.tmux_name,
        &prepared.path,
        &params.task,
        &record.id.to_string(),
    ) {
        warn!(
            id = %record.id,
            name = %record.tmux_name,
            runtime = %record.runtime.as_str(),
            "spawn_managed: runtime adapter spawn failed: {e}"
        );
        let _ = mgr
            .mark_errored(&record.id, &format!("spawn failed: {e}"))
            .await;
    } else {
        info!(
            id = %record.id,
            name = %record.tmux_name,
            path = %prepared.path.display(),
            "managed session spawned successfully"
        );
    }

    emit(ProvisioningStage::Complete);
    Ok(mgr.get(&record.id).await.unwrap_or(record))
}

/// Whether `s` names an EXISTING local directory usable as a session workspace
/// directly, i.e. without a git clone (#1433).
///
/// Why: the local-path spawn fast path must reliably distinguish "an absolute
/// directory the operator already has on disk" from "a remote repo URL to clone".
/// A URL (`https://…`, `git@…:…`) is never an absolute filesystem path, a relative
/// path is rejected (ambiguous against the daemon's cwd), and a non-existent path
/// falls through to clone — so this errs toward the safe existing behaviour.
/// What: returns `true` iff `s` is an ABSOLUTE path that is a directory. `is_dir()`
/// already implies existence in a single `stat` syscall (a missing path is not a
/// dir) and follows symlinks, so a separate `exists()` probe is redundant.
/// Test: `is_local_workdir_detects_absolute_dir`,
/// `is_local_workdir_rejects_url_relative_and_missing` in tests/local_spawn.rs.
pub fn is_local_workdir(s: &str) -> bool {
    let p = std::path::Path::new(s);
    p.is_absolute() && p.is_dir()
}

/// Write the task description to `TASK.md` in a workspace directory (both paths).
///
/// Why: the agent's initial brief must be available as a file in the workspace
/// so it can be read without interactive input (closes #1693). This helper is
/// shared by both the clone path (via `WorkspaceProvisioner::provision_in`) and
/// the local-path fast path (`spawn_managed_local`) so the two call sites cannot
/// diverge. Writing is non-fatal: a failed write is logged but never aborts the
/// spawn. Overwrite semantics are intentional — the caller's task always wins.
/// What: when `task` is non-empty, writes `task` to `<workspace>/TASK.md` and
/// logs a warning on I/O failure. When `task` is empty, does nothing (avoids
/// writing an empty file that would mislead the agent).
/// Test: `local_path_spawn_writes_task_md` in tests/local_spawn.rs.
pub fn write_task_md(workspace: &std::path::Path, task: &str, session_id: &ManagedSessionId) {
    if task.is_empty() {
        return;
    }
    let task_file = workspace.join("TASK.md");
    if let Err(e) = std::fs::write(&task_file, task) {
        tracing::warn!(
            session = %session_id,
            path = %task_file.display(),
            "failed to write TASK.md (local-path spawn): {e}"
        );
    }
}

/// Find an existing Active managed session for `source_id` whose tmux session
/// is still live, so [`spawn_managed_inproject`] can reconnect instead of
/// provisioning a duplicate worktree (#1707).
///
/// Why (issue #1931): extracted out of `spawn_managed_inproject` so the
/// reconnect PREDICATE — the exact rule the operator relies on to avoid
/// duplicate clones/worktrees for the same project — is unit-testable without
/// a live `DaemonState`, tmux, or git worktree. `find` (not the async
/// `SessionManager`/tmux calls themselves) is the only logic that can drift
/// and cause the "existing clone not detected" symptom, so this is the seam
/// worth pinning down with a hermetic regression test.
/// What: returns the first record in `records` whose `source_id` matches
/// `source_id` exactly, whose `state` is [`ManagedSessionState::Active`], and
/// whose `tmux_name` reports alive via `tmux.session_exists(...)`. `None` when
/// no record satisfies all three.
/// Test: `find_reusable_inproject_session_matches_active_live_session`,
/// `find_reusable_inproject_session_ignores_stopped_or_dead_or_other_project`.
fn find_reusable_inproject_session(
    records: &[SessionRecord],
    source_id: &str,
    tmux: &dyn crate::session_manager::ManagedTmuxDriver,
) -> Option<SessionRecord> {
    records
        .iter()
        .find(|r| {
            r.source_id.as_deref() == Some(source_id)
                && r.state == ManagedSessionState::Active
                && tmux.session_exists(&r.tmux_name)
        })
        .cloned()
}

/// Decide whether an in-project spawn should RECONNECT to an existing live
/// session rather than provision a fresh worktree (#1707 + `force_new` opt-out).
///
/// Why (issue #2450): the reconnect decision is exactly what the `force_new`
/// opt-out gates — an explicit "launch new" surface (the `tm` picker's "launch
/// new session" choice) must be able to bypass it, otherwise its task is
/// injected into an unrelated live session for the same project. Keeping the
/// decision a pure function (the `force_new` short-circuit composed with
/// [`find_reusable_inproject_session`]) makes BOTH branches — skip-when-forced
/// AND still-reconnect-when-not-forced — unit-testable without a live
/// `DaemonState`, tmux, or git worktree.
/// What: returns `None` immediately when `force_new` is set (never reconnect);
/// otherwise delegates to [`find_reusable_inproject_session`] (the unchanged
/// #1707 predicate).
/// Test: `reconnect_candidate_none_when_force_new`,
/// `reconnect_candidate_reconnects_when_not_forced`.
fn reconnect_candidate(
    force_new: bool,
    records: &[SessionRecord],
    source_id: &str,
    tmux: &dyn crate::session_manager::ManagedTmuxDriver,
) -> Option<SessionRecord> {
    if force_new {
        return None;
    }
    find_reusable_inproject_session(records, source_id, tmux)
}

/// Spawn a managed session rooted at a per-session git worktree (#1706).
///
/// Why: the in-project spawn path gives every managed session its own isolated
/// branch worktree of a protected base clone, rather than operating directly on
/// the operator's working directory. This mirrors `spawn_managed_local` but uses
/// the `worktree` path as both the session workspace and cwd, records the
/// `source_id` (`owner/repo`) on the record so `tm` can reconnect to existing
/// sessions for the same project, and sets `workspace_owned = false` (the
/// worktree is inside the base clone dir, which the operator should manage; the
/// session does NOT own it for decommission purposes).
///
/// #1913: unlike `spawn_managed`'s clone path and `spawn_managed_local`, this
/// path does not go through `WorkspaceProvisioner::provision_in` (there is no
/// clone step — the worktree already exists via `try_inproject_spawn` +
/// `reserve_inproject_worktree`), so it must call
/// [`crate::core::session_launch::prepare_session_with_repo_url`] itself.
/// Before this fix it never did, so every in-project session silently got no
/// statusline, no deployed agents/skills, no injected trusty-memory/
/// trusty-search MCP config, and no merged CLAUDE.md.
/// What: in order — (1) writes `TASK.md` into the worktree; (2) runs
/// [`prepare_inproject_session`] (best-effort, mirrors `provision_in`'s
/// non-fatal error handling); (3) creates the tmux session rooted at the
/// worktree via `create_with_reserved_name` (issue #2032 — `reserved_name` was
/// already resolved by `reserve_inproject_worktree` and used to name the
/// worktree/branch, so this step reuses it verbatim instead of re-deriving);
/// (4) sets `source_id` via `set_source_id`; (5) runs the FRONT gate
/// (fail-open); (6) marks `Active`; (7) spawns the runtime. A spawn failure
/// marks the record errored (non-fatal).
/// Test: covered transitively by the in-project spawn integration tests;
/// `prepare_inproject_session_writes_statusline` in this module's `tests`
/// submodule exercises the new prep-call in isolation (hermetic — no daemon,
/// tmux, or git required).
#[allow(clippy::too_many_arguments)]
async fn spawn_managed_inproject(
    state: &std::sync::Arc<crate::daemon::state::DaemonState>,
    session_id: &crate::session_manager::ManagedSessionId,
    params: &SpawnParams,
    runtime: crate::runtime::RuntimeKind,
    worktree: std::path::PathBuf,
    owner: String,
    repo: String,
    reserved_name: String,
) -> Result<crate::session_manager::SessionRecord, String> {
    use crate::core::provisioning_stage::{ProvisioningStage, emit};
    use crate::session_manager::ManagedSessionState;

    // NOTE (#2450): the #1707 reconnect pre-flight that used to run here has
    // been hoisted into `spawn_managed_routed`, AHEAD of the worktree
    // reservation, so a reconnect no longer creates-then-prunes a worktree and
    // the `force_new` opt-out can skip it before any side effect. By the time
    // this function runs, the caller has already decided a fresh session is
    // wanted and reserved its worktree.

    info!(
        id = %session_id,
        worktree = %worktree.display(),
        owner = %owner,
        repo = %repo,
        "spawn_managed: in-project worktree spawn"
    );

    write_task_md(&worktree, &params.task, session_id);

    // Pass a canonical GitHub HTTPS URL as repo_url so the session-manager can
    // derive the project name (`tm-<repo>-NN`, issue #1955, formerly
    // `tmpm-<repo>-<8hex>` per #1789) for the tmux session name. Using a
    // synthetic HTTPS URL is safe: `parse_github_path`
    // normalises both SSH and HTTPS forms to the same `{ owner, repo }` pair, and
    // this URL is the real origin of the base clone (it was derived from the
    // operator's local `remote.origin.url`). The record stores it as `repo_url`
    // which gives `tm session ls` useful project context even for in-project
    // sessions that did not clone a fresh workspace. It also doubles as the
    // `repo_url` threaded into `prepare_inproject_session` below, for the same
    // trusty-memory palace-pinning reason `provision_in` threads its `repo_url`.
    let synthetic_repo_url = format!("https://github.com/{owner}/{repo}");

    // Prepare the session BEFORE spawning the runtime (#1913). See the
    // function-level doc for why this call is required here specifically (no
    // clone step wraps it, unlike the other two spawn paths).
    //
    // #1931: use `for_managed_workspace(&worktree)`, NOT `default()` — the
    // harness cwd for this session IS `worktree`, so deployed agents/skills
    // must land in `<worktree>/.claude/{agents,skills}` (where Claude Code's
    // project-skill discovery looks), not the real `$HOME/.claude`.
    let fw = crate::core::paths::FrameworkPaths::for_managed_workspace(&worktree);
    prepare_inproject_session(&fw, session_id, &worktree, &synthetic_repo_url);

    // #1919: mirrors `spawn_managed_cloned`'s placement — announce the tmux
    // stage right before the record (and its tmux session name) is created.
    emit(ProvisioningStage::CreatingTmuxSession);
    let mgr = state.session_manager().await;
    let record = mgr
        .create_with_reserved_name(
            *session_id,
            reserved_name,
            params.task.clone(),
            Some(worktree.clone()),
            Some(worktree.clone()),
            Some(synthetic_repo_url),
            None,
            runtime,
            params.ephemeral.unwrap_or(false),
            false, // workspace_owned: worktrees are inside the base clone; not auto-deletable
        )
        .await
        .map_err(|e| {
            warn!(id = %session_id, "spawn_managed (inproject): create failed: {e}");
            e.to_string()
        })?;

    // Record the source project identity so callers can reconnect later.
    let source_id = format!("{owner}/{repo}");
    if let Err(e) = mgr.set_source_id(session_id, &source_id).await {
        warn!(id = %session_id, "spawn_managed (inproject): set_source_id failed: {e}");
    }

    // Front gate with the original repo_url so GitHub identity is parseable.
    if let Some(record) =
        front_gate_or_escalate(&mgr, &record, &params.repo_url, &params.task).await?
    {
        return Ok(record);
    }

    if let Err(e) = mgr
        .set_workspace(session_id, worktree.clone(), ManagedSessionState::Active)
        .await
    {
        warn!(id = %session_id, "spawn_managed (inproject): set_workspace failed: {e}");
    }

    // Deployment-completeness check (#2158, made non-blocking by #2172): see
    // `spawn_managed_cloned`'s identical check for the full rationale. Reuses
    // the `fw` already resolved above for `prepare_inproject_session`.
    if let Err(reason) =
        ensure_deployment_complete(&fw, &worktree, record.repo_url.as_deref(), session_id)
    {
        warn!(id = %session_id, "spawn_managed (inproject): deployment incomplete after auto-repair (non-blocking, launch proceeds): {reason}");
    }

    emit(ProvisioningStage::LaunchingRuntime);
    let tmux_arc = mgr.tmux_driver();
    let adapter = crate::runtime::build_adapter(record.runtime, tmux_arc);
    if let Err(e) = adapter.spawn(
        &record.tmux_name,
        &worktree,
        &params.task,
        &record.id.to_string(),
    ) {
        warn!(
            id = %record.id,
            name = %record.tmux_name,
            "spawn_managed (inproject): runtime adapter spawn failed: {e}"
        );
        let _ = mgr
            .mark_errored(&record.id, &format!("spawn failed: {e}"))
            .await;
    } else {
        info!(
            id = %record.id,
            name = %record.tmux_name,
            worktree = %worktree.display(),
            "managed session spawned successfully (in-project worktree)"
        );
    }

    emit(ProvisioningStage::Complete);
    Ok(mgr.get(&record.id).await.unwrap_or(record))
}

/// Run the session-preparation pipeline for an in-project worktree, logging
/// (never propagating) any failure.
///
/// Why (#1913): [`spawn_managed_inproject`] has no clone step to wrap this call
/// in — unlike `spawn_managed`'s clone branch and `spawn_managed_local`, which
/// both get preparation "for free" as part of `WorkspaceProvisioner::provision_in`
/// — so it must invoke [`crate::core::session_launch::prepare_session_with_repo_url`]
/// directly. Extracted to a named, `fw`-parameterised function (rather than
/// inlined) so it is unit-testable against a hermetic [`crate::core::paths::FrameworkPaths::under`]
/// tempdir without touching the operator's real `~/.trusty-mpm`/`~/.claude`.
/// What: calls `prepare_session_with_repo_url(fw, worktree, Some(repo_url))`.
/// On success, logs the deployed-agent count AND the deployed-skill count
/// (`report.skill_deploy.deployed.len()` — #1917; previously only the agent
/// count was logged, so a skill-deploy no-op was invisible here too). On
/// failure, logs a `tracing::warn!` and returns — mirroring
/// `WorkspaceProvisioner::provision_in`'s non-fatal handling of the identical
/// call, so a prep failure never blocks the session from spawning.
/// Test: `prepare_inproject_session_writes_statusline` in this module's `tests`
/// submodule.
fn prepare_inproject_session(
    fw: &crate::core::paths::FrameworkPaths,
    session_id: &ManagedSessionId,
    worktree: &std::path::Path,
    repo_url: &str,
) {
    match crate::core::session_launch::prepare_session_with_repo_url(fw, worktree, Some(repo_url)) {
        Ok(report) => {
            info!(
                id = %session_id,
                deployed = report.deploy.deployed.len(),
                skills_deployed = report.skill_deploy.deployed.len(),
                worktree = %worktree.display(),
                "spawn_managed (inproject): session prepared"
            );
            // Issue #2149: a roster-deploy failure no longer aborts
            // preparation, so surface it loudly here rather than letting it
            // hide behind a low `deployed`/`skills_deployed` count.
            for err in &report.roster_errors {
                tracing::error!(
                    id = %session_id,
                    worktree = %worktree.display(),
                    "spawn_managed (inproject): roster provisioning gap (session \
                     still launches with its trusty-mpm identity): {err}"
                );
            }
        }
        Err(e) => {
            warn!(
                id = %session_id,
                worktree = %worktree.display(),
                "spawn_managed (inproject): session prep failed (non-fatal): {e}"
            );
        }
    }
}

/// Spawn a managed session rooted at a local directory, redirecting to a managed
/// clone when the directory has a parseable GitHub remote (#1590).
///
/// Why: the local-path fast path of [`spawn_managed`]. Before #1590 this function
/// used the live checkout directly; now it checks for a GitHub remote and, when
/// found, provisions a managed clone under the canonical
/// `~/trusty-mpm-projects/<owner>/<repo>/<session-id>/` path — keeping the live
/// checkout untouched. A local directory with NO parseable GitHub remote is an
/// error: managed sessions always operate on a remote clone so concurrent sessions
/// are isolated from the operator's working tree.
/// What: in order — (0) reads `remote.origin.url` from the local directory; if
/// absent or unparseable, returns `Err` (the `tm connect` path handles remotes-less
/// directories); (1) provisions a managed clone via `provision_in` and reassigns
/// `workspace` to the clone path; (2) creates the tmux session record via
/// `create_with_id` (with `cwd = workspace_path = <managed clone>`,
/// `repo_url = Some(<origin_url>)`, `owned = true`); (3) sets `source_id` on the
/// record; (4) runs the FRONT gate (fail-open); (5) marks the record `Active`;
/// (6) spawns the runtime. A spawn failure marks the record errored (non-fatal).
/// Returns the final record.
/// Test: `spawn_managed_local_redirects_to_managed_clone` and
/// `spawn_managed_local_errors_on_no_remote` in tests/local_spawn.rs cover the
/// two key branches. The clone path assertions live in the existing
/// `local_path_spawn_*` test suite.
async fn spawn_managed_local(
    state: &Arc<DaemonState>,
    session_id: &ManagedSessionId,
    params: &SpawnParams,
    runtime: RuntimeKind,
) -> Result<SessionRecord, String> {
    use crate::core::provisioning_stage::{ProvisioningStage, emit};

    let local_dir = std::path::PathBuf::from(&params.repo_url);

    // Step 0 (#1590): managed-path redirect.
    //
    // Check whether the local directory has a parseable GitHub remote. If it does,
    // provision a managed clone and operate in that clone instead of the live
    // checkout. If it does not, the managed path cannot be established — error so
    // the caller (or the operator via `tm connect`) handles the no-remote case.
    let origin_url = super::inproject::get_origin_url(&local_dir).ok_or_else(|| {
        format!(
            "spawn failed: '{}' has no git origin remote; \
                 managed sessions require a GitHub remote. \
                 Use `tm connect` / `tm launch --live` to run in the live checkout.",
            local_dir.display()
        )
    })?;

    let gh = trusty_common::github_path::parse_github_path(&origin_url).ok_or_else(|| {
        format!(
            "spawn failed: could not parse a GitHub owner/repo from origin remote \
             '{origin_url}' for '{}'. \
             Use `tm connect` to run in the live checkout instead.",
            local_dir.display()
        )
    })?;

    let source_id_str = format!("{}/{}", gh.owner, gh.repo);
    let config = crate::core::trusty_tools_config::TrustyToolsConfig::load();
    let project_dir = crate::core::trusty_tools_config::workspace_subpath(&config, &gh);
    // #2184: same per-project identity resolution as the clone-based path
    // (`spawn_managed_cloned`) — this branch also provisions a managed clone
    // (of `origin_url`), so it must honour the same binding.
    let git_identity = crate::core::git_identity::resolve_for_config(&config, &origin_url)
        .map_err(|e| {
            warn!(id = %session_id, "spawn_managed (local→managed): git identity resolution failed: {e}");
            format!("git identity resolution failed: {e}")
        })?;
    let provisioner = crate::provisioner::WorkspaceProvisioner::new(
        crate::provisioner::RealGitBackend::new(git_identity),
        std::path::PathBuf::new(),
    );
    let prepared = provisioner
        .provision_in(
            &project_dir,
            session_id,
            &origin_url,
            &params.git_ref,
            &params.task,
        )
        .map_err(|e| {
            warn!(id = %session_id, "spawn_managed (local→managed): provision failed: {e}");
            format!("workspace provisioning failed: {e}")
        })?;

    // `workspace` now points at the MANAGED clone, not the live checkout.
    let workspace = prepared.path;
    info!(
        id = %session_id,
        live = %local_dir.display(),
        managed = %workspace.display(),
        source_id = %source_id_str,
        "spawn_managed: local-path redirected to managed clone (#1590)"
    );

    // #1919: mirrors `spawn_managed_cloned`'s placement — the clone/prepare
    // stages above already fired inside `provision_in`; announce the tmux
    // stage right before the record (and its tmux session name) is created.
    emit(ProvisioningStage::CreatingTmuxSession);
    let mgr = state.session_manager().await;
    let record = mgr
        .create_with_id(
            *session_id,
            params.task.clone(),
            Some(workspace.clone()),
            params.name_hint.clone(),
            Some(workspace.clone()),
            Some(origin_url.clone()),
            if params.git_ref.is_empty() {
                None
            } else {
                Some(params.git_ref.clone())
            },
            runtime,
            params.ephemeral.unwrap_or(false),
            true, // owned: we provisioned a fresh clone; decommission may remove it
        )
        .await
        .map_err(|e| {
            warn!(id = %session_id, "spawn_managed (local→managed): create failed: {e}");
            e.to_string()
        })?;

    // Record the source project identity so callers can reconnect by project.
    if let Err(e) = mgr.set_source_id(session_id, &source_id_str).await {
        warn!(id = %session_id, "spawn_managed (local→managed): set_source_id failed: {e}");
    }

    // FRONT gate: origin_url is a real GitHub URL so the gate is active.
    if let Some(record) = front_gate_or_escalate(&mgr, &record, &origin_url, &params.task).await? {
        return Ok(record);
    }

    if let Err(e) = mgr
        .set_workspace(&record.id, workspace.clone(), ManagedSessionState::Active)
        .await
    {
        warn!(id = %record.id, "spawn_managed (local→managed): set_workspace failed: {e}");
    }

    // Deployment-completeness check (#2158, made non-blocking by #2172): see
    // `spawn_managed_cloned`'s identical check for the full rationale.
    let fw = crate::core::paths::FrameworkPaths::for_managed_workspace(&workspace);
    if let Err(reason) =
        ensure_deployment_complete(&fw, &workspace, record.repo_url.as_deref(), &record.id)
    {
        warn!(id = %record.id, "spawn_managed (local→managed): deployment incomplete after auto-repair (non-blocking, launch proceeds): {reason}");
    }

    emit(ProvisioningStage::LaunchingRuntime);
    let tmux_arc = mgr.tmux_driver();
    let adapter = build_adapter(record.runtime, tmux_arc);
    if let Err(e) = adapter.spawn(
        &record.tmux_name,
        &workspace,
        &params.task,
        &record.id.to_string(),
    ) {
        warn!(
            id = %record.id,
            name = %record.tmux_name,
            runtime = %record.runtime.as_str(),
            "spawn_managed (local→managed): runtime adapter spawn failed: {e}"
        );
        let _ = mgr
            .mark_errored(&record.id, &format!("spawn failed: {e}"))
            .await;
    } else {
        info!(
            id = %record.id,
            name = %record.tmux_name,
            path = %workspace.display(),
            "managed session spawned successfully (local→managed clone)"
        );
    }

    emit(ProvisioningStage::Complete);
    Ok(mgr.get(&record.id).await.unwrap_or(record))
}

/// Perform the withheld spawn (Step 3) for a FRONT-gate-escalated session.
///
/// Why: when the FRONT gate escalates (#1360), the spawn is WITHHELD and the
/// session sits in `Provisioning` with a `pending_decision`. After a human
/// resolves it via `POST …/answer`, the runtime must actually start — otherwise
/// the answer clears the decision but the agent never launches (AC-15). This is
/// the exact Step 3 from [`spawn_managed`], lifted so the answer path can invoke
/// it on demand.
/// What: transitions the record to `Active` (persisting the workspace), builds
/// the runtime adapter, and spawns the harness in the pane — marking the record
/// errored on spawn failure (non-fatal, mirroring `spawn_managed`). Idempotent
/// guard: callers should only invoke this for a session still in `Provisioning`
/// (never spawned), so an already-live session is not double-spawned.
/// Test: `front_gate_answer_unblocks_spawn` in tests/session_manager_mvp.rs.
pub async fn spawn_runtime_for(
    state: &Arc<DaemonState>,
    record: &SessionRecord,
) -> Result<(), String> {
    let mgr = state.session_manager().await;
    let workspace = record
        .workspace_path
        .clone()
        .unwrap_or_else(|| record.cwd.clone());

    if let Err(e) = mgr
        .set_workspace(&record.id, workspace.clone(), ManagedSessionState::Active)
        .await
    {
        warn!(id = %record.id, "spawn_runtime_for: set_workspace failed: {e}");
    }

    let tmux_arc = mgr.tmux_driver();
    let adapter = build_adapter(record.runtime, tmux_arc);
    if let Err(e) = adapter.spawn(
        &record.tmux_name,
        &workspace,
        &record.task,
        &record.id.to_string(),
    ) {
        warn!(
            id = %record.id,
            name = %record.tmux_name,
            "spawn_runtime_for: runtime adapter spawn failed: {e}"
        );
        let _ = mgr
            .mark_errored(&record.id, &format!("spawn failed: {e}"))
            .await;
        return Err(e.to_string());
    }
    info!(
        id = %record.id,
        name = %record.tmux_name,
        "FRONT-gate-escalated session spawned after human approval"
    );
    Ok(())
}

/// Run the intent-conformance FRONT gate for a freshly-created session record.
///
/// Why: factored out of [`spawn_managed`] so the gate's escalate-vs-proceed
/// decision (and the #1269 degraded operator-confirm branch) is a single, named,
/// testable step rather than inlined in the spawn ritual (spec §5.1, #1360). It
/// keeps `spawn_managed` linear and under the 500-SLOC production cap.
/// What: derives owner/repo from the repo URL, builds the production
/// [`IsrConformanceGate`] rooted at the provisioned workspace, composes the
/// conformance disposition with the pre-work autonomy disposition (stricter-wins,
/// via [`run_front_gate`]), and —
/// - on **auto-accept** → returns `Ok(None)`; the caller proceeds to spawn;
/// - on **escalate** → writes `pending_decision`/`proposed_default` (so the
///   escalation surfaces through `…/activity`, MCP `session_status`, the
///   supervisor, and the `tm` CLI), withholds the spawn, logs the divergence,
///   and returns `Ok(Some(record))` so the caller returns early WITHOUT spawning.
///
/// #1269 degradation: until the headless auto-spawn approval path lands
/// ([`HeadlessApproval::current`] returns `OperatorConfirm`), an escalation
/// degrades to the operator-confirm path — the pending decision is recorded and a
/// human resolves it via `POST …/answer` (which then unblocks the spawn). The
/// gate itself always functions; only the *resolution channel* is degraded.
///
/// Fail-open: the gate never `Err`s on its own resolution failures (it returns an
/// `AutoAccept`); this function only returns `Err` if persisting the escalation
/// fails — in which case the caller surfaces the store error rather than silently
/// spawning.
/// Test: `front_gate_escalation_sets_pending_decision` /
/// `front_gate_clean_match_spawns` in tests/session_manager_mvp.rs, plus the unit
/// matrix in `managed_routes::front_gate::tests`.
async fn front_gate_or_escalate(
    mgr: &Arc<crate::session_manager::SessionManager>,
    record: &SessionRecord,
    repo_url: &str,
    task: &str,
) -> Result<Option<SessionRecord>, String> {
    use super::front_gate::{
        HeadlessApproval, IsrConformanceGate, prework_autonomy_disposition, run_front_gate,
    };

    let (owner, repo) = match trusty_common::github_path::parse_github_path(repo_url) {
        Some(gh) => (gh.owner, gh.repo),
        // No GitHub identity → no ticket backend to resolve against; fail-open.
        None => return Ok(None),
    };

    let repo_root = record
        .workspace_path
        .clone()
        .unwrap_or_else(|| record.cwd.clone());
    let gate = IsrConformanceGate::new(repo_root);
    let autonomy = prework_autonomy_disposition(task);
    let approval = HeadlessApproval::current();

    let outcome = run_front_gate(&gate, &owner, &repo, task, autonomy, approval).await;

    if outcome.may_spawn() {
        return Ok(None);
    }

    let reason = outcome
        .escalation_reason()
        .unwrap_or("conformance escalation")
        .to_string();
    warn!(
        id = %record.id,
        approval = ?outcome.approval,
        "spawn_managed: FRONT gate escalated; withholding spawn: {reason}"
    );
    mgr.set_pending_decision(&record.id, &reason, outcome.proposed_default.as_deref())
        .await
        .map_err(|e| {
            warn!(id = %record.id, "spawn_managed: set_pending_decision failed: {e}");
            e.to_string()
        })?;

    // Return the updated record (now carrying the pending decision), withholding
    // the spawn. The session stays in `Provisioning` until a human answers.
    Ok(Some(
        mgr.get(&record.id).await.unwrap_or_else(|_| record.clone()),
    ))
}

/// Typed failure modes for [`resume_managed`], shared across transports.
///
/// Why: the prior design mapped resume failures to HTTP status codes by
/// substring-matching the `Display` string (`msg.contains("invalid state
/// transition")` → 409, `msg.contains("session not found")` → 404), which
/// silently regressed to 500 the moment any error wording changed. A typed enum
/// lets the HTTP handler match on variants (→ 404/409/500) with no stringly-typed
/// coupling, and lets the MCP path render a stable `Display` string whose
/// "not found" substring the existing MCP tests rely on.
/// What: five variants — `NotFound` (the id is absent), `InvalidState` (the
/// session is not `Stopped`/`Errored`, carrying the descriptive reason),
/// `WorkspaceGone` and `PaneGone` (both operator-actionable on-disk
/// preconditions that make a resume impossible even though the request is
/// well-formed — split into DISTINCT variants, not one shared `Unresumable`,
/// because their safe remedies differ: see each variant's doc), and `Other`
/// (any remaining genuinely-internal failure: store/I-O). The `Display`
/// strings are chosen so the not-found variant still contains the literal
/// "not found".
/// Test: `resume_managed_typed_*` in tests/session_manager_mvp.rs drive the
/// 404/409/422 paths through the typed value (no `Display` matching), and the
/// MCP `session_resume_unknown_id_errors` test asserts the rendered string.
#[derive(Debug, thiserror::Error)]
pub enum ResumeManagedError {
    /// The requested session id was not present in the store → HTTP 404.
    #[error("session not found: {0}")]
    NotFound(String),

    /// The session is not in a resumable state (only `Stopped`/`Errored` are) →
    /// HTTP 409. Carries the manager's descriptive reason.
    #[error("invalid state transition: {0}")]
    InvalidState(String),

    /// The session's workspace directory was removed
    /// ([`ManagedError::WorkspaceMissing`]) → HTTP 422. Carries the manager's
    /// full actionable message (names the vanished path).
    ///
    /// Why (#2577): a removed workspace is an OPERATOR-actionable precondition,
    /// not a daemon-internal fault — routing it through `Other` → 500 gave the
    /// CLI a bare "daemon returned an internal error (500)" with no clue the
    /// worktree had simply been removed. Kept as its own variant (not merged
    /// with `PaneGone` under one `Unresumable`) because its safe remedy is
    /// different: with no workspace left to protect, `tm session delete
    /// --force` (store-only, never touches tmux) is safe here — the SAME verb
    /// would be actively dangerous for `PaneGone` (see that variant's doc).
    #[error("{0}")]
    WorkspaceGone(String),

    /// The session's recorded tmux pane vanished while a SIBLING window keeps
    /// the tmux session alive ([`ManagedError::PaneGone`]) → HTTP 422. Carries
    /// the manager's full actionable message (names the vanished pane id).
    ///
    /// Why (#2577 review): this is the #2467/#2468 sibling-window-hijack
    /// protection firing — the tmux SESSION is still alive and may hold other
    /// live work; `tm session decommission` kills the WHOLE session (the live
    /// sibling included). A prior draft merged this with `WorkspaceGone` under
    /// one `Unresumable` variant and pointed BOTH at the same "just delete it"
    /// remedy — factually wrong here, since there is nothing missing to
    /// justify teardown, only a stale pane reference. Kept distinct so the CLI
    /// can render a remedy that tells the operator to INSPECT
    /// (`tmux list-panes`) before doing anything destructive.
    #[error("{0}")]
    PaneGone(String),

    /// Any other genuinely-internal failure (store/I-O) → HTTP 500.
    #[error("{0}")]
    Other(String),
}

impl From<ManagedError> for ResumeManagedError {
    /// Why: `SessionManager::resume` returns a typed [`ManagedError`]; mapping its
    /// variants here (rather than at each call site) keeps the not-found/invalid-state
    /// HTTP distinction in one place and prevents a wording change from regressing
    /// a 404/409 to a 500.
    /// What: maps `SessionNotFound` → `NotFound`, `InvalidState` → `InvalidState`
    /// (preserving the descriptive reason), `WorkspaceMissing` → `WorkspaceGone`,
    /// `PaneGone` → `PaneGone` (each preserving the manager's full actionable
    /// Display message verbatim), and every remaining variant → `Other`.
    /// Test: covered transitively by the resume handler 404/409/422 tests
    /// (`resume_managed_typed_*` in tests/session_manager_mvp.rs).
    fn from(e: ManagedError) -> Self {
        match e {
            ManagedError::SessionNotFound(id) => ResumeManagedError::NotFound(id),
            ManagedError::InvalidState(_, reason) => ResumeManagedError::InvalidState(reason),
            // The Display impls of these two variants already carry the vanished
            // path/pane and the concrete remedy — preserve them verbatim so the
            // 422 body is fully actionable at the CLI.
            e @ ManagedError::WorkspaceMissing(..) => {
                ResumeManagedError::WorkspaceGone(e.to_string())
            }
            e @ ManagedError::PaneGone(..) => ResumeManagedError::PaneGone(e.to_string()),
            other => ResumeManagedError::Other(other.to_string()),
        }
    }
}

/// Resume a stopped session and re-spawn its runtime, shared across transports.
///
/// Why: the HTTP resume handler and the MCP `session_resume` tool must both
/// resume the record AND re-spawn the runtime so the session is actually live;
/// centralising avoids the MCP path silently resuming without re-spawning.
/// What: calls [`crate::session_manager::SessionManager::resume`] (which performs
/// the existence + state check in a SINGLE round-trip — no pre-flight `get`, so
/// no TOCTOU window) and maps its typed [`ManagedError`] into a typed
/// [`ResumeManagedError`] (`NotFound`/`InvalidState`/`Other`). It then re-spawns
/// the SAME runtime backend in the fresh tmux session (no re-clone) and returns
/// the final record.
///
/// #1913 self-heal: sessions spawned via the (now-fixed) in-project worktree
/// path before this fix landed never ran `prepare_session` at all, so their
/// workspace may be permanently missing the `statusLine` config key. Every
/// resume defensively re-applies [`crate::core::session_launch::ensure_status_line`]
/// — the ONE prep step confirmed idempotent/non-clobbering by its own doc
/// comment — so such a session self-heals the next time it is resumed. The
/// broader prep pipeline (agent/skill redeploy, CLAUDE.md merge, MCP injection)
/// is intentionally NOT re-run here: those steps are not all confirmed safe to
/// repeat against an already-running workspace, so re-running them on every
/// resume risks a different class of bug for a narrower payoff.
/// Test: covered by the HTTP `resume_managed_session` tests and the MCP
/// `session_resume_unknown_id_errors` test;
/// `resume_managed_backfills_missing_status_line` in
/// `tests/session_manager_mvp.rs` covers the self-heal call added here.
pub async fn resume_managed(
    state: &Arc<DaemonState>,
    id: &ManagedSessionId,
) -> Result<SessionRecord, ResumeManagedError> {
    let mgr = state.session_manager().await;
    let record = mgr.resume(id).await.map_err(ResumeManagedError::from)?;

    let workspace = record
        .workspace_path
        .clone()
        .unwrap_or_else(|| record.cwd.clone());

    // Defensive self-heal (#1913): best-effort, never blocks the resume.
    if let Err(e) = crate::core::session_launch::ensure_status_line(&workspace) {
        warn!(
            id = %record.id,
            "resume_managed: statusline self-heal failed (non-fatal): {e}"
        );
    }

    // Deployment-completeness check (#2158, made non-blocking by #2172): see
    // `spawn_managed_cloned`'s identical check for the full rationale.
    // `ensure_deployment_complete` itself no-ops for an unresolved (`/unknown`)
    // workspace — an adopted session with no known cwd is handled separately by
    // the reconcile-on-boot fix, not here.
    let fw = crate::core::paths::FrameworkPaths::for_managed_workspace(&workspace);
    if let Err(reason) =
        ensure_deployment_complete(&fw, &workspace, record.repo_url.as_deref(), &record.id)
    {
        warn!(id = %record.id, "resume_managed: deployment incomplete after auto-repair (non-blocking, launch proceeds): {reason}");
    }

    let tmux_arc = mgr.tmux_driver();
    let adapter = build_adapter(record.runtime, tmux_arc);
    // #1744: prefer --resume <id> when a claude_session_id was captured at
    // SessionStart; fall back to --continue (most-recent conversation in the
    // workspace) when the id is absent. ClaudeCodeAdapter overrides spawn_resume
    // to implement this; TcodeAdapter's default delegates to plain spawn.
    // Sibling-window hijack fix (follow-up to #2456): pass the record's OWN
    // `record.pane_id` through so the adapter targets that SPECIFIC pane
    // (`SessionManager::resume`, just above, already confirmed it — or a
    // freshly recreated one — still exists) instead of a session-scoped
    // target that tmux could resolve to an unrelated active sibling pane.
    if let Err(e) = adapter.spawn_resume(
        &record.tmux_name,
        record.pane_id.as_deref(),
        &workspace,
        &record.task,
        record.claude_session_id.as_deref(),
        &record.id.to_string(),
    ) {
        warn!(
            id = %record.id,
            name = %record.tmux_name,
            runtime = %record.runtime.as_str(),
            "resume_managed: runtime adapter spawn_resume failed: {e}"
        );
        let _ = mgr
            .mark_errored(&record.id, &format!("resume spawn failed: {e}"))
            .await;
    } else {
        info!(
            id = %record.id,
            name = %record.tmux_name,
            workspace = %workspace.display(),
            "managed session resumed and runtime respawned"
        );
    }

    Ok(mgr.get(id).await.unwrap_or(record))
}

/// Validate a workspace against the canonical bundled roster before handing
/// the session to the operator, auto-repairing first when gaps are found
/// (issue #2158).
///
/// Why: `prepare_session_inner`'s roster/output-style/hooks steps are already
/// best-effort/non-fatal (issue #2149) so a session always launches carrying
/// SOME identity — but "launches" is not the same as "launches complete". A
/// worktree whose `.claude/` payload came up incomplete (missing agents, a
/// stripped `settings.json`, no ownership manifest — see #2158) could
/// otherwise silently reach the operator. This function surfaces that gap:
/// validate, and if incomplete, re-run the deploy pipeline once via
/// [`crate::core::deploy_validate::validate_and_repair`] (which reuses
/// [`crate::core::session_launch::prepare_session_with_repo_url`] — the exact
/// #2149 pipeline, no parallel repair implementation), then re-validate.
/// **Non-blocking as of #2172 (P0):** every `spawn_managed_*`/`resume_managed`
/// call site now treats `Err` as a `tracing::warn!`-only diagnostic and always
/// proceeds to `adapter.spawn`/`adapter.spawn_resume` regardless of the
/// result. The original #2158 contract — skip the runtime launch and mark the
/// record errored on `Err` — turned out to be unsafe to wire as a hard gate:
/// the validator over-reports INCOMPLETE (#2171), so the gate was aborting
/// `adapter.spawn` on effectively every new/restarted managed session,
/// leaving the pane at a bare shell. This function's return type is
/// deliberately still `Result<(), String>` (callers/tests still want the
/// pass/fail detail to log or assert on) — it is the CALLERS' responsibility
/// to never let that `Err` skip the launch. Do not reintroduce an early
/// return/`mark_errored` on this `Err` at any call site without first fixing
/// #2171 and re-litigating whether a hard gate is safe.
/// What: no-ops (`Ok(())`) when `workspace` is the adopted-session sentinel
/// `/unknown` or does not exist on disk — an unresolved workspace has nothing
/// to validate; that case is handled separately by the `reconcile_on_boot`
/// adopted-session fix, not here. Otherwise delegates to `validate_and_repair`
/// using the caller-resolved `fw` (production call sites pass
/// [`crate::core::paths::FrameworkPaths::for_managed_workspace`]`(workspace)`;
/// tests inject a hermetic [`crate::core::paths::FrameworkPaths::under`]).
/// `Ok(())` when the workspace is (or becomes) complete; `Err(detail)` naming
/// every residual gap otherwise.
/// Test: `ensure_deployment_complete_noops_for_unknown_workspace`,
/// `ensure_deployment_complete_ok_when_already_complete`,
/// `ensure_deployment_complete_repairs_and_succeeds`;
/// `spawn_managed_cloned_launches_despite_incomplete_deployment` (added by
/// #2172) asserts the call site itself no longer skips `adapter.spawn` on
/// `Err`.
fn ensure_deployment_complete(
    fw: &crate::core::paths::FrameworkPaths,
    workspace: &std::path::Path,
    repo_url: Option<&str>,
    session_id: &ManagedSessionId,
) -> Result<(), String> {
    if workspace == std::path::Path::new("/unknown") || !workspace.is_dir() {
        return Ok(());
    }
    let outcome = crate::core::deploy_validate::validate_and_repair(fw, workspace, repo_url);
    // Warn-only carrier-reachability self-check (issue #2231) — see its own
    // doc comment. Runs regardless of the completeness verdict below and can
    // NEVER turn this `Ok` branch into an `Err`; it only logs.
    warn_if_no_persona_carrier(&outcome.after.gaps, workspace, session_id);
    if outcome.before.is_complete() {
        return Ok(());
    }
    if outcome.is_complete() {
        info!(
            id = %session_id,
            gaps = outcome.before.gaps.len(),
            "deployment validation: auto-repair closed all gaps before handoff"
        );
        return Ok(());
    }
    let detail: Vec<String> = outcome.after.gaps.iter().map(|g| g.describe()).collect();
    Err(format!(
        "deployment incomplete after auto-repair ({} gap(s) remain): {}",
        detail.len(),
        detail.join("; ")
    ))
}

/// Warn-only self-check: is at least one delegation-persona CARRIER reachable
/// under the daemon path's `--setting-sources project,local` posture (issue
/// #2231)?
///
/// Why: `--setting-sources project,local` (see
/// `core::model_inject::SETTING_SOURCES_FLAG`) restricts the launched
/// `claude` to the project+local tiers and EXCLUDES the `user` tier that
/// `CLAUDE_CONFIG_DIR` relocates to (see `core::managed_config`'s module doc,
/// "WHICH LAYER ACTUALLY LOADS THE ROSTER") — so the PM's identity survives
/// ONLY if a project-tier carrier is reachable: either the deployed
/// `trusty-mpm` output-style file (`settings.json`'s `outputStyle` resolving
/// to a real file under `.claude/output-styles/`), or the per-workspace
/// instructions stash (`<workspace>/.trusty-mpm/last-instructions.md`,
/// written by `session_launch::prepare_session_inner` and injected via
/// `--append-system-prompt-file`). This is a DIAGNOSTIC ONLY, mirroring the
/// #2172/98b994c3 lesson this very function already embodies
/// (`ensure_deployment_complete` was softened from a hard gate to a
/// non-blocking warn by #2172, commit 98b994c3) — over-reporting "no carrier
/// reachable" must NEVER abort a real launch, so this only logs; it cannot
/// fail this function or any caller, and it never returns a value.
/// What: logs `tracing::warn!` with an actionable message (naming the missing
/// carriers and how they are normally wired) when [`carrier_reachable`]
/// returns `false`; a no-op otherwise.
/// Test: `carrier_reachable_*` cover the pure predicate directly;
/// `ensure_deployment_complete_does_not_abort_when_no_carrier_reachable`
/// asserts this self-check never turns the caller's `Ok` into an `Err`.
fn warn_if_no_persona_carrier(
    gaps: &[crate::core::deploy_validate::DeploymentGap],
    workspace: &std::path::Path,
    session_id: &ManagedSessionId,
) {
    if carrier_reachable(gaps, workspace) {
        return;
    }
    warn!(
        id = %session_id,
        workspace = %workspace.display(),
        "deployment self-check: no delegation-persona carrier reachable under \
         --setting-sources project,local (no project-tier output-style file \
         resolved from settings.json's outputStyle, and no \
         .trusty-mpm/last-instructions.md prompt stash) — the launched PM may \
         be missing its identity/instructions carrier; this is diagnostic only \
         and does not block the launch (issue #2231). Re-run `tm doctor` or \
         `tm repair` against this workspace to re-provision the .claude/ payload."
    );
}

/// Pure predicate: is at least one delegation-persona carrier reachable?
///
/// Why: isolated from the logging side effect in
/// [`warn_if_no_persona_carrier`] so the decision itself is directly
/// unit-testable — see that function's doc for the full carrier-reachability
/// rationale.
/// What: `true` when `gaps` contains NONE of the output-style-related
/// [`crate::core::deploy_validate::DeploymentGap`] variants
/// (`OutputStyleKeyMissing`, `OutputStyleUnknownId`, `OutputStyleFileMissing`)
/// — the output-style carrier is intact — OR
/// `<workspace>/.trusty-mpm/last-instructions.md` exists and is non-empty (the
/// prompt-file carrier). `false` only when NEITHER carrier is reachable.
/// Test: `carrier_reachable_true_when_no_output_style_gap`,
/// `carrier_reachable_true_when_prompt_file_present_despite_style_gap`,
/// `carrier_reachable_false_when_neither_carrier_present`.
fn carrier_reachable(
    gaps: &[crate::core::deploy_validate::DeploymentGap],
    workspace: &std::path::Path,
) -> bool {
    use crate::core::deploy_validate::DeploymentGap;

    let output_style_ok = !gaps.iter().any(|g| {
        matches!(
            g,
            DeploymentGap::OutputStyleKeyMissing
                | DeploymentGap::OutputStyleUnknownId(_)
                | DeploymentGap::OutputStyleFileMissing(_)
        )
    });
    if output_style_ok {
        return true;
    }
    workspace
        .join(".trusty-mpm")
        .join("last-instructions.md")
        .metadata()
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
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
        let emitter =
            StageEmitter::new(session_id.to_string(), "https://github.com/owner/repo", tx);

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

        let mut fw =
            crate::core::paths::FrameworkPaths::for_managed_project(tmp.path(), &workspace);
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
}
