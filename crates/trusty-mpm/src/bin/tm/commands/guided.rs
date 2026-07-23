//! Bare `tm` guided default mode — session picker (#1705, #1708).
//!
//! Why: typing `tm` alone — with no subcommand — should do the most useful
//! thing for the operator's current context. Inside a GitHub-backed git
//! repository with a reachable daemon, the guided default (#1705) queries the
//! daemon for existing sessions bound to the detected `owner/repo`, shows them
//! in a numbered list, and lets the operator resume or launch with a single key.
//! When the daemon is unreachable or the project is not GitHub-backed, the
//! protected fallback (#1724) ensures the live checkout is never touched.
//!
//! What: [`run_guided_default`] orchestrates detection, listing, and dispatch.
//! [`derive_project`] derives the `source_id`, managed workspace, and git root.
//! [`fallback_protected`] is the three-way dispatch for the daemon-unreachable
//! path; it is also the target for non-GitHub projects. [`non_github_refusal_message`]
//! is the pure helper that produces the accurate refusal text for the
//! non-GitHub-remote case (see #1777). The TTY picker is split into a pure
//! `parse_picker_choice` + an I/O driver `run_tty_picker`.
//!
//! Test: unit tests for detection and fallback live in `tests_behavior_b_tests.rs`
//! (#1724 regression suite) and `tests_behavior_c_tests.rs` (#1705 new UX suite);
//! the managed-spawn integration path is exercised by `tests/session_manager_mvp.rs`.

use anyhow::Context as _;
use std::io::IsTerminal as _;

pub(crate) use super::guided_autostart::github_host;
// #3714 line-cap follow-up: the resolver + tie-break logic moved to the
// sibling `guided_resolver` module (mirrors `guided_autostart`/
// `guided_inplace`'s own extraction pattern) — imported here (not
// re-exported) since `nested_managed_match` is only called internally by
// `nested_session_guard` below; nothing outside this crate's tests
// addresses it via `guided::nested_managed_match`.
use super::guided_resolver::nested_managed_match;
use crate::formatters::info_box::DaemonInfo;

// ── Entry point ──────────────────────────────────────────────────────────────

/// Bare `tm` guided default — detect project, list sessions, and offer a picker.
///
/// Why: operators who type `tm` from inside a project directory should not
/// have to remember `tm session new`, `tm connect`, or `tm attach <name>` —
/// the guided default derives the project identity, shows existing sessions,
/// and lets them resume or launch in one step. The live checkout is NEVER
/// written to; all managed sessions run in the protected base-clone workspace.
/// What: (0) an operator-supplied `--url` / `TRUSTY_MPM_URL` that is
/// unreachable errors out immediately (issue #1737 — see below) before any
/// of the following runs; (1) tries the rich picker UX when the daemon is
/// reachable and the CWD is a GitHub-backed git project; (2) falls through to
/// [`fallback_protected`] for every other case (daemon unreachable, non-GitHub
/// remote, non-git directory). Non-TTY piped invocations print project + session
/// info and exit 0 without hanging for input (#1705 AC-7). Subdir launches
/// pass the git root as `repo_url` so the daemon sets `source_id` correctly.
///
/// #1737: `url` is the already-resolved string every caller downstream of
/// here uses (unchanged); `explicit` carries the ORIGINAL `Option::is_some`
/// signal — `Some` iff the operator actually supplied `--url` /
/// `TRUSTY_MPM_URL` (never a clap default, per the #2487 discipline — see
/// `core::discovery`'s module doc). This function is the one LIVE,
/// empirically-confirmed instance of the #1737 silent-fallback bug:
/// [`super::guided_autostart::ensure_daemon_started`]'s auto-start step
/// intentionally ignores whatever URL it is handed and always re-resolves
/// IMPLICITLY (lock file → default) once *some* daemon is confirmed healthy
/// — exactly right for the ordinary "daemon not started yet" first-run UX
/// this function exists to smooth over, but it means an unreachable EXPLICIT
/// `--url` silently ended up auto-starting (or reconnecting to) a completely
/// different, real daemon and showing ITS live session list instead of
/// erroring. Probing `explicit` up front — before in-place relaunch, the
/// nested-session guard, the picker, or auto-start ever run — closes that
/// gap without touching the implicit (unset URL) path at all.
/// Test: `guided_derive_project_returns_none_for_non_git_dir`,
/// `guided_non_tty_gate_returns_false_skips_stdin`,
/// `guided_fallback_never_pollutes_github_git_checkout` (#1724);
/// `tests/tm_guided_default_explicit_url.rs` end-to-end (#1737).
pub(crate) async fn run_guided_default(
    client: &reqwest::Client,
    url: &str,
    explicit: Option<&str>,
) -> anyhow::Result<()> {
    // #1737: verify an EXPLICIT URL's own reachability before anything below
    // gets a chance to silently auto-start or reconnect to a DIFFERENT
    // daemon. `resolve_daemon_url_for_cli` is a no-op passthrough to `Ok`
    // when `explicit` is `None`/empty (implicit resolution, unaffected). The
    // failure message is printed here (mirroring
    // `commands::prune::prune_idle`'s `PruneError::SmUnavailable` handling)
    // so `main`'s top-level exit-code translation only needs to downcast and
    // `process::exit` — no double-print.
    if explicit.is_some_and(|u| !u.is_empty())
        && let Err(err) = trusty_mpm::core::resolve_daemon_url_for_cli(client, explicit).await
    {
        eprintln!("tm: {err}");
        return Err(err.into());
    }

    // #2023 component C: an in-pane relaunch takes priority over EVERYTHING
    // below — project detection, the picker, the daemon-unreachable fallback.
    // `TM_MANAGED_SESSION_ID` (exported by #2023 component B into the pane's
    // shell) only resolves to a known managed session when bare `tm` is
    // literally running inside that session's own pane after its runtime
    // exited; any other case (unset, blank, stale/unknown to the daemon)
    // falls through to the ordinary guided default below.
    if let Some(result) = super::guided_inplace::try_inplace_relaunch(client, url).await {
        return result;
    }

    // #2157 item 4: nested-session guard. `try_inplace_relaunch` above only
    // fires when bare `tm` runs in THIS EXACT pane after its runtime exited
    // (env var resolved + record Stopped). A sibling pane/window in the SAME
    // tmux session is a different case entirely: no env var to key off, the
    // record may still be Active, yet launching a brand-new session from here
    // would nest one tmux client's session inside another (the root cause of
    // #2157). Check BEFORE any project detection or picker logic runs.
    if let Some(record) = nested_session_guard(client, url).await {
        // #2453: `record.state` can lag reality by up to 60s after the
        // pane's `claude` process exits (the periodic reap tick / a racing
        // `SessionEnd` hook has not caught up yet). Previously this branch
        // trusted that stale state unconditionally and fell straight to
        // `resume_guided_session`, which — because the tmux SESSION is
        // (correctly) still alive — always resolved to a `switch-client`
        // no-op: the operator was already inside the very session being
        // "reconnected" to.
        //
        // #2456 review finding 1 (CRITICAL, ROUND 2): `nested_session_guard`
        // matches on the tmux SESSION name (shared by every window/pane in
        // that session) — NOT proof this is the pane bound to `record`. A
        // ROUND-1 fix compared the process-level `TM_MANAGED_SESSION_ID` env
        // var, reasoning it was pane/process-scoped; that premise was
        // EMPIRICALLY DISPROVEN (live tmux 3.6b): the healing step in
        // `SessionManager::mark_runtime_exited_stopped` calls `tmux
        // set-environment -t <session>` — SESSION-scoped — and tmux applies
        // a session's stored environment to the process env of every NEW
        // pane/window created in that session AFTERWARD. Any session that
        // has exited-and-relaunched once therefore has a "poisoned" session
        // env that a genuinely different sibling window inherits into its
        // OWN process env — the env-based gate would have passed that
        // sibling. The authoritative signal is tmux's own stable `pane_id`
        // (never inherited across panes, unlike an env var or `pane_pid`,
        // which the OS can reuse across a pane's lifetime) — see
        // `pane_identity_confirmed`'s doc for the full rationale. A record
        // with no captured `pane_id` (legacy, pre-#2453) or a tmux query
        // failure both fail CLOSED — never trust a weaker signal to justify
        // the destructive `run_inplace_relaunch` `exec`, which lands in
        // WHATEVER pane this process is literally running in, chdir'd to
        // `record`'s workspace.
        let current_pane_id = super::tmux_attach::current_tmux_pane_id();

        if pane_identity_confirmed(current_pane_id.as_deref(), record.pane_id.as_deref()) {
            // Safe to attempt: its daemon reactivate call independently
            // reconciles a stale-`Active` record whose pane is confirmed
            // idle (#2453 daemon-side fix, `daemon::managed_routes::
            // reactivate::reconcile_stale_active_then_reactivate`) — and, as
            // of #2789, no longer 409s just because the requesting `tm` is
            // this pane's own foreground command (the caller's pane_id is
            // forwarded so the daemon treats it as idle). It still refuses
            // when the runtime is genuinely live in a SIBLING window.
            //
            // #2794: we are HERE only because `pane_identity_confirmed` matched
            // the stable tmux `pane_id`, so this process is inside `record`'s
            // OWN pane. `pane_id` identity alone is not PROOF the agent has
            // exited — any child process of the pane (including a live agent
            // that shelled out to run bare `tm`) inherits the same `pane_id` —
            // so this is the ordinary interactive-relaunch case: the operator's
            // shell has handed this pane back to bare `tm`, which is what
            // happens when the agent has exited. We forward that as a
            // self-asserted claim (`pane_confirmed_dead = true`, NOT
            // independently verified by the daemon) so it can reconcile a
            // zombie-`Active` record whose pane still reads busy (the requesting
            // `tm`, or a not-yet-reaped child) instead of 409-dead-ending — the
            // reported bug. The claim is bounded, not a bare pass: an
            // INDEPENDENT sibling-pane liveness check on the daemon still
            // refuses when the runtime is genuinely live in a SIBLING window.
            match super::guided_inplace::run_inplace_relaunch(
                client,
                url,
                &record.id,
                record.clone(),
                current_pane_id.as_deref(),
                true,
            )
            .await
            {
                super::guided_inplace::InPlaceOutcome::Result(Ok(())) => return Ok(()),
                super::guided_inplace::InPlaceOutcome::Result(Err(e)) => {
                    // #2789: we are provably inside THIS record's OWN pane
                    // (`pane_identity_confirmed`). The pre-existing fallback —
                    // `resume_guided_session` → `switch-client -t <this very
                    // session>` — is a guaranteed NO-OP that strands the
                    // operator in the bare shell they are already looking at
                    // (the reported dead-end). Report the failure honestly and
                    // tell them how to relaunch the agent in THIS pane, instead
                    // of the misleading "switching client" no-op. A failure
                    // here — pre-mutation (cwd/`claude`-binary resolution) or
                    // the `exec` itself — must NOT hard-fail (exit non-zero),
                    // matching the #2456 finding-2 contract for this call site.
                    eprintln!("tm: in-place relaunch failed ({e})");
                    eprintln!("{}", inplace_self_relaunch_hint(&record));
                    return Ok(());
                }
                super::guided_inplace::InPlaceOutcome::FallThrough => {
                    // #2789: the daemon still declined to reconcile — a
                    // genuinely live sibling window keeps the tmux session
                    // busy (the correct #2157 refusal), or the daemon predates
                    // the caller-pane reconcile. Either way a switch-client to
                    // the session this pane already belongs to is a no-op, so
                    // do NOT offer it; give the honest self-relaunch hint.
                    eprintln!(
                        "tm: in-place relaunch unavailable for '{}' (state={})",
                        record.name, record.state
                    );
                    eprintln!("{}", inplace_self_relaunch_hint(&record));
                    return Ok(());
                }
            }
        }

        // Session-name-only match: the operator is in the SAME tmux session but
        // NOT (provably) the record's own pane. #2777: split on liveness.
        match nested_fallback_action(&record.state) {
            // Dead session (decommissioned/stopped/errored): a switch-client
            // reconnect to the session the operator is already inside is a
            // guaranteed no-op dead-end (the reported bug). Relaunch the agent in
            // THIS pane via the same daemon-authorized primitive the
            // pane-confirmed branch above uses. Safe WITHOUT pane-identity
            // confirmation precisely because a dead session has no live runtime
            // anywhere to hijack (the whole point of that gate) — and the
            // daemon's `reactivate` round-trip remains the sole authority: a
            // refusal folds into `FallThrough` → an honest self-relaunch hint,
            // never an unconditional exec.
            NestedFallbackAction::RelaunchInPlace => {
                let current_pane_id = super::tmux_attach::current_tmux_pane_id();
                // #2794: this is a session-name-only match (pane identity NOT
                // confirmed), so we have no proof-of-death for a specific pane —
                // pass `false` and let the daemon's ordinary probe decide. The
                // dead states that reach here (stopped/decommissioned) reactivate
                // directly anyway; `errored` falls to the probe path unchanged.
                match super::guided_inplace::run_inplace_relaunch(
                    client,
                    url,
                    &record.id,
                    record.clone(),
                    current_pane_id.as_deref(),
                    false,
                )
                .await
                {
                    super::guided_inplace::InPlaceOutcome::Result(Ok(())) => return Ok(()),
                    super::guided_inplace::InPlaceOutcome::Result(Err(e)) => {
                        eprintln!("tm: in-place relaunch failed ({e})");
                        eprintln!("{}", inplace_self_relaunch_hint(&record));
                        return Ok(());
                    }
                    super::guided_inplace::InPlaceOutcome::FallThrough => {
                        eprintln!(
                            "tm: in-place relaunch unavailable for '{}' (state={})",
                            record.name, record.state
                        );
                        eprintln!("{}", inplace_self_relaunch_hint(&record));
                        return Ok(());
                    }
                }
            }
            // Live session: a sibling window may hold the live runtime, so a
            // switch-client reconnect legitimately brings it forward — not a self
            // no-op. Preserve the pre-existing refuse+reconnect behavior.
            // Terminal entry point (not a loop) — the `AttachOutcome` only matters
            // to a looping caller like `run_tty_picker` (#2678); here, the
            // hand-off (or fail-closed skip) is this call's whole job, so discard it.
            NestedFallbackAction::Reconnect => {
                eprintln!(
                    "tm: this tmux session ('{}') is already a managed session (state={}) — {}",
                    record.name,
                    record.state,
                    nested_guard_notice(&record.name)
                );
                super::guided_resume::resume_guided_session(client, url, &record).await?;
                return Ok(());
            }
        }
    }

    let cwd = std::env::current_dir().context("cannot resolve current directory")?;
    let workdir = cwd.to_string_lossy().to_string();
    eprintln!("tm: no subcommand — using guided default for {workdir}");

    // Auto-register the git project root as a local path alias (non-fatal, silent).
    // Why: every `tm` invocation from a git directory populates `tm ls` with
    // the project's canonical name so operators can quickly find their projects.
    super::managed_root::try_register_alias(&cwd);

    // Use a mutable URL so that a successful auto-start can update it to the
    // actual bound address discovered via the lock file.
    let mut effective_url = url.to_string();

    // Try the rich UX when CWD is a GitHub-backed git project.
    if let Some((source_id, workspace, git_root)) = derive_project(&cwd) {
        // Pass the git root as repo_url so daemon detects .git at root even
        // when tm is invoked from a subdirectory (#1705 LOW fix).
        let repo_url = git_root.to_string_lossy().to_string();

        // First attempt: daemon may already be up.
        if let Some(r) = try_show_picker(
            client,
            &effective_url,
            &source_id,
            &workspace,
            &repo_url,
            &cwd,
        )
        .await
        {
            return r;
        }

        // Daemon unreachable — try to auto-start it transparently.
        eprintln!("tm: daemon not running — starting it…");
        match super::guided_autostart::ensure_daemon_started(client, &effective_url).await {
            Ok(new_url) => {
                effective_url = new_url;
                // Retry the full picker flow with the freshly-started daemon.
                if let Some(r) = try_show_picker(
                    client,
                    &effective_url,
                    &source_id,
                    &workspace,
                    &repo_url,
                    &cwd,
                )
                .await
                {
                    return r;
                }
                eprintln!("tm: daemon started but sessions still unreachable; falling back");
            }
            Err(e) => eprintln!("tm: auto-start failed ({e}); falling back to offline mode"),
        }
    }

    // Daemon unreachable OR not a GitHub project: protected fallback (#1724).
    // For GitHub projects this redirects to the managed-clone workspace.
    // For non-GitHub git projects it refuses (live-checkout guard).
    // For non-git directories it falls through to the classic `tm launch` path.
    fallback_protected(client, &effective_url, &cwd).await
}

/// Attempt to list sessions and display the interactive picker for a GitHub project.
///
/// Why: the same "list sessions → two-panel-banner → picker" sequence is needed
/// both on the initial attempt and after a successful auto-start. A shared helper
/// keeps `run_guided_default` DRY and avoids divergence between the two call sites.
/// What: fetches the project's live sessions via the shared
/// [`super::session_picker::fetch_live_sessions`] (decommissioned tombstones
/// already filtered, #1809), runs the tty-gate, renders the two-panel daily
/// banner (#1808), then hands off to [`super::session_picker::run_tty_picker`].
/// Returns `None` when the daemon is unreachable so the caller can try
/// auto-start; returns `Some(result)` once the daemon responded.
/// Test: indirectly covered by guided-default e2e tests; the pure sub-functions
/// (`tty_gate`, `parse_picker_choice`, `is_live_session_state`) are unit-tested
/// independently.
async fn try_show_picker(
    client: &reqwest::Client,
    url: &str,
    source_id: &str,
    workspace: &std::path::Path,
    repo_url: &str,
    cwd: &std::path::Path,
) -> Option<anyhow::Result<()>> {
    // #1809: exclude decommissioned tombstones from the picker by default. The
    // shared fetch path returns the same live-only list the static renderer uses.
    let sessions = super::session_picker::fetch_live_sessions(client, url, Some(source_id), false)
        .await
        .ok()?;
    if !tty_gate(
        std::io::stdin().is_terminal(),
        source_id,
        workspace,
        &sessions,
    ) {
        return Some(Ok(()));
    }
    // #1808: render the same two-panel banner as `tm banner` — version in the
    // title bar, 24-row clipped art, project/workspace fields — no sleep.
    let daemon = DaemonInfo::from_lock_file_with_probe().with_count(sessions.len());
    crate::formatters::banner::print_daily_banner(&cwd.to_string_lossy(), &daemon);
    let scope = super::session_picker::PickerScope::project(source_id, repo_url);
    Some(super::session_picker::run_tty_picker(client, url, &scope, sessions).await)
}

// ── Project derivation ───────────────────────────────────────────────────────

/// Derive the GitHub `source_id`, managed workspace, and git root from `cwd`.
///
/// Why: the picker needs the `owner/repo` identity (for filtering sessions by
/// `source_id`), the managed workspace path (for display), and the git root
/// path (to pass as `repo_url` so the daemon sets `source_id` correctly even
/// when `tm` is invoked from a subdirectory — #1705 LOW fix).
/// What: finds a *usable* git root (via [`usable_git_root`] — an ancestor `.git`
/// is trusted only when `cwd` is genuinely part of that repo's tracked tree, so
/// an untracked directory nested inside another repo does NOT inherit that
/// repo's identity — #2534), reads `remote.origin.url`, guards that it is a
/// GitHub URL, parses `owner/repo` via `parse_github_path`, and returns
/// `(source_id, workspace_path, git_root)` where `source_id = "owner/repo"`,
/// `workspace_path = ~/trusty-mpm-projects/<owner>/<repo>` (the canonical base),
/// and `git_root` is the absolute path of the repository root.
/// Test: `guided_derive_project_returns_none_for_non_git_dir`,
/// `guided_derive_project_rejects_non_github_remote`,
/// `guided_derive_project_accepts_github_https_remote`,
/// `guided_derive_project_returns_some_from_subdir`,
/// `guided_derive_project_returns_none_for_untracked_ancestor_subdir`.
pub(crate) fn derive_project(
    cwd: &std::path::Path,
) -> Option<(String, std::path::PathBuf, std::path::PathBuf)> {
    use trusty_mpm::daemon::managed_routes::inproject;
    let git_root = usable_git_root(cwd)?;
    let origin_url = inproject::get_origin_url(&git_root).filter(|u| is_github_remote(u))?;
    let gh = trusty_common::github_path::parse_github_path(&origin_url)?;
    let source_id = format!("{}/{}", gh.owner, gh.repo);
    let workspace = inproject::base_clone_path(&gh.owner, &gh.repo);
    Some((source_id, workspace, git_root))
}

// ── Nested-session guard (#2157 item 4) ──────────────────────────────────────

/// I/O driver for the nested-session guard: resolve the current tmux context,
/// fetch every managed session (unfiltered — a record's `source_id` may be
/// missing, see #2157 item 5), and apply [`nested_managed_match`].
///
/// Why: extracted from [`run_guided_default`] so the "gather the two matching
/// keys, ask the daemon, apply the pure predicate" sequence is one readable
/// unit. Fetches the FULL session list (no `?source_id=` filter) rather than
/// reusing `list_project_sessions` — the whole point of this guard is to catch
/// a managed record the project-filtered list would MISS (e.g. one whose
/// `source_id` write never landed, #2157 item 5's failure mode).
/// What: short-circuits to `None` when not inside tmux (nothing to guard
/// against) or when the daemon is unreachable/errors (fail-open — matches
/// every other daemon round-trip in this file, which falls through to the
/// picker/autostart rather than blocking on a down daemon). Uses a 2-second
/// timeout, matching the `guided_inplace` probe convention.
/// Test: the pure decision is `nested_managed_match_*` in
/// `tests_behavior_c_tests.rs`; this I/O wrapper is exercised by manual smoke
/// tests and the e2e suite (requires a live daemon + tmux).
async fn nested_session_guard(
    client: &reqwest::Client,
    url: &str,
) -> Option<trusty_mpm::client::ManagedSessionSummary> {
    if !super::tmux_attach::inside_tmux() {
        return None;
    }
    let current_session_name = super::tmux_attach::current_tmux_session_name();
    let env_session_id = std::env::var("TM_MANAGED_SESSION_ID")
        .ok()
        .filter(|v| !v.trim().is_empty());
    // #3714: resolved here (not only later, at the pane-identity-confirmed
    // check) so it can also break a duplicate-name tie in `nested_managed_match`
    // itself — see that function's doc.
    let current_pane_id = super::tmux_attach::current_tmux_pane_id();
    let all = list_all_managed_sessions(client, url).await.ok()?;
    nested_managed_match(
        current_session_name.as_deref(),
        env_session_id.as_deref(),
        current_pane_id.as_deref(),
        &all,
    )
    .cloned()
}

/// Pure predicate (#2456 review finding 1, ROUND 2 — the round-1 env-var
/// gate was empirically disproven): does the CURRENT pane's own stable tmux
/// `pane_id` confirm THIS pane is the one bound to `record_pane_id` — as
/// opposed to [`nested_managed_match`] having matched purely on tmux SESSION
/// name (a signal every window/pane in that session shares)?
///
/// Why: the round-1 fix compared `TM_MANAGED_SESSION_ID` process env values,
/// reasoning that env var was pane/process-scoped. That premise was proven
/// FALSE against a live tmux 3.6b: `mark_runtime_exited_stopped`'s healing
/// step calls `tmux set-environment -t <session>` — SESSION-scoped — and
/// tmux applies a session's stored environment to the process env of every
/// NEW pane/window created in that session AFTERWARD. tmux's own `pane_id`
/// (e.g. `"%5"`, distinct from `pane_pid`, which the OS can reuse across a
/// pane's lifetime) is NEVER inherited across panes — see
/// [`SessionRecord::pane_id`](trusty_mpm::session_manager::SessionRecord::pane_id)'s
/// doc for the full rationale and [`super::tmux_attach::current_tmux_pane_id`]
/// for how the CURRENT pane's id is resolved.
/// What: moved to [`super::pane_identity::pane_identity_confirmed`] (issue
/// #3600) — that module is now the ONE shared cross-check primitive used by
/// this guard AND by `rename.rs`/`pm_guard_deny_by_default.rs`, so the same
/// hijack this guard fixed here doesn't rot back into a divergent (or
/// missing) copy elsewhere. Re-exported so existing imports of
/// `crate::commands::guided::pane_identity_confirmed` keep resolving.
/// Test: see `super::pane_identity`'s module doc for the full test list.
pub(crate) use super::pane_identity::pane_identity_confirmed;

/// The action the nested-session guard takes when it matched a record by tmux
/// SESSION name but could NOT confirm pane-level identity
/// ([`pane_identity_confirmed`] was `false`).
///
/// Why: the pre-#2777 fallback ALWAYS reconnected (switch-client) to the matched
/// session. That is correct when the session has a live runtime in a sibling
/// window — the switch brings it forward. But when the session is DEAD
/// (decommissioned/stopped/errored), there is no live runtime anywhere in that
/// tmux session, so a switch-client to the session the operator is already
/// inside is a guaranteed no-op dead-end (the reported bug: bare `tm` inside a
/// `decommissioned` `tm-apex-01` printed "switching client to session
/// 'tm-apex-01'" and stranded the operator in the bare shell). Splitting the two
/// cases lets the dead case relaunch the agent in place instead.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NestedFallbackAction {
    /// The matched session has no live runtime — relaunch the agent in the
    /// current pane (a reconnect would be a self no-op).
    RelaunchInPlace,
    /// The matched session may have a live runtime in a sibling window —
    /// switch-client reconnect brings it forward.
    Reconnect,
}

/// Pure decision for the nested-guard fallback: relaunch-in-place vs reconnect,
/// derived solely from the matched record's `state`.
///
/// Why: isolating the classification from the daemon/tmux I/O makes the
/// dead-vs-live decision exhaustively unit-testable (#2777), mirroring the
/// `switch_decision`-style pure seam pattern (#2680).
/// What: dead/tombstone states with no live runtime
/// (`decommissioned`/`stopped`/`errored`) →
/// [`NestedFallbackAction::RelaunchInPlace`]; every other state
/// (`active`/`provisioning`/`running`/unknown) → the safe default
/// [`NestedFallbackAction::Reconnect`], preserving the pre-#2777 behavior for a
/// possibly-live sibling window.
/// Test: `nested_fallback_action_relaunches_dead_states`,
/// `nested_fallback_action_reconnects_live_states` in `tests_behavior_c_tests.rs`.
pub(crate) fn nested_fallback_action(state: &str) -> NestedFallbackAction {
    match state {
        "decommissioned" | "stopped" | "errored" => NestedFallbackAction::RelaunchInPlace,
        _ => NestedFallbackAction::Reconnect,
    }
}

/// Build the operator-facing notice for the nested-session guard's fallback
/// path — reused by both call sites that print it before falling through to
/// [`super::guided_resume::resume_guided_session`].
///
/// Why: the two call sites previously printed "…refusing to launch a nested
/// session here." as a standalone line, immediately followed by a SEPARATE
/// "reconnecting to '{name}' instead…" line. When the subsequent reconcile
/// (or plain restart) succeeds, that reads as a contradiction: the operator
/// sees "refusing" — which sounds like a hard failure — immediately followed
/// by a session that comes up fine anyway. Nothing was actually refused: `tm`
/// deliberately chose the reconnect path over nesting a new session, and that
/// choice went on to work. Folding both lines into one, reworded message
/// removes the false "this failed" signal without hiding WHY a fresh session
/// was not launched here.
/// What: returns the single-sentence notice — "not nesting a new session
/// here; reconnecting to '{name}' instead…" — that replaces both the old
/// "refusing…" line and the old separate "reconnecting…" line.
/// Test: `nested_guard_notice_never_says_refusing`,
/// `nested_guard_notice_mentions_reconnect_target`.
pub(crate) fn nested_guard_notice(name: &str) -> String {
    format!("not nesting a new session here; reconnecting to '{name}' instead…")
}

/// Actionable hint (#2789, corrected #2794) printed when bare `tm` runs inside
/// a managed session's OWN pane (pane identity confirmed) but the in-place
/// relaunch could not proceed.
///
/// Why: reconnecting to the very session whose pane the operator is already
/// inside is a `switch-client` no-op that strands them in a bare shell — the
/// reported dead-end (`tm-cto-01` transcript). When the in-place relaunch
/// cannot run (a genuinely live sibling window, a daemon predating the
/// caller-pane reconcile, or a failed `exec`), the honest response is to tell
/// the operator how to relaunch the agent themselves. #2794: the prior wording
/// suggested a bare `claude` (or `claude --resume <id>`), which is WRONG — a
/// raw `claude` launches OUTSIDE the managed session, dropping the tm-owned
/// `CLAUDE_CONFIG_DIR`, the agent roster, and the persona (the DOC-34 managed
/// config). The managed relaunch is `tm sessions resume <id>`, which re-spawns
/// the runtime with the full managed environment intact — that is the only
/// correct advice, so the hint always points there.
/// What: a single-line hint suggesting `tm sessions resume <managed-id>`
/// (`record.id`, the managed session id — NOT the claude conversation id). Pure
/// — takes an already-fetched record, returns the string.
/// Test: `inplace_self_relaunch_hint_suggests_managed_resume`,
/// `inplace_self_relaunch_hint_never_suggests_bare_claude`.
pub(crate) fn inplace_self_relaunch_hint(
    record: &trusty_mpm::client::ManagedSessionSummary,
) -> String {
    format!(
        "tm: you are already inside this session's pane — its agent has exited; \
         relaunch it as a managed session with:  tm sessions resume {}",
        record.id
    )
}

/// Fetch EVERY managed session, unfiltered — `GET /api/v1/sessions/managed`
/// with no `?source_id=` query.
///
/// Why: [`nested_session_guard`] must find a match even when the record's
/// `source_id` is missing (#2157 item 5's failure mode), so it cannot reuse the
/// source_id-filtered picker fetch ([`super::session_picker::fetch_live_sessions`]).
/// What: GETs the managed-list endpoint with no `?source_id=` query and a
/// 2-second timeout so a hung daemon cannot add a long stall to every bare
/// `tm` invocation made from inside tmux.
/// Test: I/O path; requires a live daemon.
async fn list_all_managed_sessions(
    client: &reqwest::Client,
    url: &str,
) -> anyhow::Result<Vec<trusty_mpm::client::ManagedSessionSummary>> {
    let resp = client
        .get(format!("{url}/api/v1/sessions/managed"))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await?
        .error_for_status()?;
    let body: trusty_mpm::client::ManagedListResponse = resp.json().await?;
    Ok(body.sessions)
}

// ── Display / TTY-gate helpers ────────────────────────────────────────────────

/// Print project context and decide whether to run the interactive picker.
///
/// Why: a testable seam over `std::io::stdin().is_terminal()`. By injecting
/// `is_tty`, callers in tests can exercise the non-TTY branch without a live
/// stdin — confirming that the branch returns `false` (no picker needed) and
/// thus prevents any attempt to read from stdin (#1705 AC-7 / HIGH-2b).
/// What: always calls [`print_project_context`]. If `is_tty = false`, also
/// calls [`print_non_tty_hint`] and returns `false`. If `is_tty = true`,
/// returns `true` (caller should invoke [`super::session_picker::run_tty_picker`]).
/// Test: `guided_non_tty_gate_returns_false_skips_stdin`,
/// `guided_tty_gate_returns_true_for_tty`.
pub(crate) fn tty_gate(
    is_tty: bool,
    source_id: &str,
    workspace: &std::path::Path,
    sessions: &[trusty_mpm::client::ManagedSessionSummary],
) -> bool {
    print_project_context(source_id, workspace, sessions);
    if !is_tty {
        print_non_tty_hint(source_id, sessions);
    }
    is_tty
}

/// Format one project-context session row (#3741: no `tm:` prefix, leads
/// with the `[N]` index — matching `session_picker_render::format_session_row`'s
/// convention for the interactive picker's rows).
///
/// Why: extracted to a pure, directly assertable function rather than only
/// exercised via `print_project_context`'s `eprintln!` side effect — the
/// #3741 code-critic finding was exactly this row silently drifting back to
/// the old `tm:   [N] …` shape with no test pinning its exact text.
/// What: `[{i+1}] {name}  state={state}  last={activity}` — `i+1` is this
/// listing's plain 1-based position (deliberately NOT the picker's stable
/// `shown_slot` numbering; unifying the two is out of scope for #3741).
/// Test: `guided_format_project_context_row_*` in `tests_behavior_c_tests.rs`.
pub(crate) fn format_project_context_row(
    index: usize,
    s: &trusty_mpm::client::ManagedSessionSummary,
) -> String {
    let activity = s.last_activity_at.as_deref().unwrap_or("—");
    format!(
        "[{}] {}  state={}  last={activity}",
        index + 1,
        s.name,
        s.state
    )
}

/// Print the detected project and session list to stderr.
///
/// Why: the operator needs to see at a glance which project was detected, where
/// the managed workspace lives (so it is clear the live checkout is untouched),
/// and which sessions are available before being prompted.
/// What: prints the source_id, workspace path, and a numbered session list (or
/// "(none)" when empty). All output goes to stderr (stdout stays clean). Row
/// text comes from [`format_project_context_row`] (#3741: no `tm:` prefix).
/// Test: `guided_print_project_context_does_not_panic_no_sessions`,
/// `guided_print_project_context_does_not_panic_with_sessions`,
/// `guided_format_project_context_row_*`.
pub(crate) fn print_project_context(
    source_id: &str,
    workspace: &std::path::Path,
    sessions: &[trusty_mpm::client::ManagedSessionSummary],
) {
    eprintln!("tm: project:   {source_id}");
    eprintln!(
        "tm: workspace: {} (live checkout is NOT touched)",
        workspace.display()
    );
    if sessions.is_empty() {
        eprintln!("tm: sessions:  (none)");
    } else {
        eprintln!("tm: sessions:");
        for (i, s) in sessions.iter().enumerate() {
            eprintln!("{}", format_project_context_row(i, s));
        }
    }
}

/// Print a non-TTY degradation notice and actionable hints.
///
/// Why: when stdin is not a TTY (CI, pipes, scripts), hanging for input would
/// block the caller forever; print the context and exit cleanly instead (#1705 AC-7).
/// What: emits one-line notice + resume/launch hints to stderr. The caller
/// returns `Ok(())` immediately after.
/// Test: `guided_print_non_tty_hint_does_not_panic_no_sessions`,
/// `guided_print_non_tty_hint_does_not_panic_with_sessions`,
/// `guided_non_tty_gate_returns_false_skips_stdin`.
pub(crate) fn print_non_tty_hint(
    source_id: &str,
    sessions: &[trusty_mpm::client::ManagedSessionSummary],
) {
    eprintln!("tm: (stdin is not a TTY — run `tm` from an interactive terminal to use the picker)");
    if sessions.is_empty() {
        eprintln!("tm: to launch a new session: start the daemon and run `tm` from a TTY");
    } else {
        let n = sessions.len();
        eprintln!("tm: {n} session(s) found for {source_id}");
        eprintln!("tm: to resume: tmux attach-session -t {}", sessions[0].name);
        eprintln!("tm: to launch: run `tm` from an interactive terminal");
    }
}

// ── Guided resume/restart ────────────────────────────────────────────────────
//
// The resume/restart flow (zombie auto-reconcile, daemon /resume, attach) lives
// in the sibling `guided_resume` module (#2001) to keep both files under the
// 500-SLOC production cap. The picker calls
// `super::guided_resume::resume_guided_session`; the pure seams
// (`needs_restart`, `is_zombie`, `plan_resume`, `ResumeAction`) are unit-tested
// there directly.

// ── Fallback (daemon-unreachable / non-GitHub) path ─────────────────────────

/// Build the user-facing refusal notice for a non-GitHub-remote refusal.
///
/// Why: the non-GitHub-remote branch of [`fallback_protected`] previously
/// printed "daemon unreachable" wording, which is wrong when the daemon IS
/// running — the refusal is purely because `tm` only auto-manages GitHub
/// repositories. Extracting the message into a pure helper makes it
/// unit-testable without capturing stderr.
/// What: returns a three-line notice that names the non-GitHub remote as the
/// reason for refusal and reassures the operator that the live checkout was
/// not modified. The caller prints via `eprintln!` and then bails.
/// Test: `guided_non_github_refusal_message_*` in `tests_behavior_c_tests.rs`.
pub(crate) fn non_github_refusal_message(remote_desc: &str) -> String {
    format!(
        "tm: not auto-managing this project — `tm` auto-manages GitHub repositories only.\n\
         tm: detected non-GitHub remote: {remote_desc}.\n\
         tm: your live checkout was not touched. \
         (Use an explicit `tm` subcommand to work here manually.)"
    )
}

/// Return true when the remote URL targets GitHub (any transport or SSH alias).
///
/// Why: `parse_github_path` from `trusty-common` accepts *any* git remote URL,
/// not just GitHub ones. We need a host-specific guard so that non-GitHub git
/// projects (Gitea, GitLab, bare SSH, etc.) are refused rather than having a
/// clone attempt made against an unrecognised host (#1724 residual gap). We
/// also need to recognise GitHub repos accessed via multi-account SSH host
/// aliases (e.g. `git@github-duetto:owner/repo.git` via `~/.ssh/config`).
/// What: extracts the host via [`github_host`], then accepts it when:
///   (a) host == `"github.com"` (the canonical GitHub host), OR
///   (b) host starts with `"github-"` or `"github_"` (SSH alias convention —
///       e.g. `github-duetto`, `github-work`, `github_personal`).
/// Decision — GitHub Enterprise (`github.mycompany.com`): NOT matched. GHE
/// uses a dot separator after `github`, and the broader managed-clone
/// infrastructure is not tested against GHE; erring on the side of safety
/// avoids an unexpected redirect for GHE users.
/// Does NOT match: `githubusercontent.com` (the `u` after `github` is
/// alphanumeric, so neither rule fires), `gitlab.com`, `bitbucket.org`, or
/// any other host.
/// Test: `is_github_remote_*` unit tests in `tests_behavior_c_tests.rs`;
/// `guided_fallback_never_pollutes_github_git_checkout` (real `github.com` URL);
/// `guided_fallback_blocks_non_github_git_checkout` (Gitea URL must be blocked).
pub(crate) fn is_github_remote(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let host = github_host(&lower);
    host == "github.com" || host.starts_with("github-") || host.starts_with("github_")
}

/// Detect the git working-tree root at any depth by shelling to git.
///
/// Why: `cwd.join(".git").exists()` only fires when `cwd` IS the repo root.
/// Operators commonly run `tm` from a nested subdirectory (e.g. `~/project/src/`);
/// the shallow check would miss the repo, bypass the live-checkout guard, and
/// allow `launch(None)` to write framework files into that subdir (#1724 gap).
/// What: runs `git -C <cwd> rev-parse --show-toplevel`; returns the absolute
/// repo root on success, `None` when cwd is not inside any git working tree
/// (plain directory or bare repo). Bare repos (`--git-dir` succeeds but
/// `--show-toplevel` does not) are treated as non-git — deploying into a bare
/// repo has no live-checkout to protect.
/// Test: `guided_fallback_blocks_github_git_from_subdirectory` calls
/// `fallback_protected` from a nested subdir and asserts the protection fires.
fn find_git_root(cwd: &std::path::Path) -> Option<std::path::PathBuf> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let root = String::from_utf8_lossy(&out.stdout);
    let root = root.trim();
    (!root.is_empty()).then_some(std::path::PathBuf::from(root))
}

// ── Ancestor-repo trust guard (#2534) ────────────────────────────────────────

/// Classification of `cwd` with respect to the nearest enclosing git working tree.
///
/// Why: [`find_git_root`] happily walks *up* to an ancestor `.git`. That is
/// correct for a subdirectory the operator actually committed, but WRONG when
/// `cwd` is an unrelated directory that merely happens to live inside another
/// repository's working-tree span — e.g. an untracked notes folder `~/Duetto/cto`
/// that case-folds onto the enclosing repo's tracked `CTO/` dir on a
/// case-insensitive filesystem (APFS). Trusting the ancestor there made bare
/// `tm` silently resolve to the ancestor repo's project and offer its sessions
/// (#2534). This enum lets both `cwd → project` resolution paths distinguish the
/// three cases with a single git query.
/// What: `Usable` = `cwd` is the repo root itself, or a subdirectory that IS part
/// of the tracked tree — safe to inherit the repo's identity. `UntrackedInsideAncestor`
/// = `cwd` is a strict subdirectory of a git working tree but is NOT in its
/// tracked tree — must be treated as a non-project. `NotGit` = `cwd` is not inside
/// any git working tree.
/// Test: `guided_classify_cwd_usable_for_tracked_subdir`,
/// `guided_classify_cwd_untracked_for_uncommitted_subdir`,
/// `guided_classify_cwd_not_git_for_plain_dir`.
#[derive(Debug)]
pub(crate) enum CwdProject {
    /// `cwd` belongs to this git root's tracked tree (or IS the root).
    Usable(std::path::PathBuf),
    /// `cwd` sits inside this git root's working tree but is not tracked by it.
    UntrackedInsideAncestor(std::path::PathBuf),
    /// `cwd` is not inside any git working tree.
    NotGit,
}

/// Does `cwd` directly own a `.git` entry — i.e. is `cwd` itself a working-tree
/// root, whose identity must never be inherited from an enclosing ancestor?
///
/// Why: the own-repo-wins guard (#2542). A `.git` entry is either a directory
/// (normal repository) or a *file* (a linked-worktree / submodule gitlink whose
/// content is `gitdir: <path>`) — BOTH mean `cwd` is a working-tree root. The
/// match MUST be on the EXACT entry name `.git`: a `.git`-prefixed sibling such as
/// `.git.hotstats-backup-20260713` (a preserved gitdir from a rescue) is NOT a
/// repo marker and must be ignored, so a `starts_with(".git")`/glob test would be
/// a bug. `Path::join(".git")` composes the exact name and `try_exists` follows a
/// `.git` symlink to its target while still reporting a broken/absent entry as
/// not-owned.
/// What: returns `true` when `cwd/.git` exists as any file-system entry (dir,
/// file, or a symlink resolving to one), `false` when it is absent or a query
/// error occurs (fail-open to the git-walk path — never falsely claim ownership).
/// Test: `guided_cwd_owns_git_entry_true_for_dir`,
/// `guided_cwd_owns_git_entry_true_for_pointer_file`,
/// `guided_cwd_owns_git_entry_false_for_dotgit_prefixed_sibling`,
/// `guided_cwd_owns_git_entry_false_when_absent`.
pub(crate) fn cwd_owns_git_entry(cwd: &std::path::Path) -> bool {
    cwd.join(".git").try_exists().unwrap_or(false)
}

/// Classify `cwd` against its nearest enclosing git working tree, verifying
/// tracked-tree membership before trusting an *ancestor* repo's identity.
///
/// Why: closes the ancestor-`.git` trust gap (#2534) AND the inverse gap where a
/// directory that DOES own its repo was wrongly adopted into an ancestor (#2542).
/// `find_git_root` alone cannot tell an intentionally-nested tracked subdirectory
/// apart from an unrelated directory that merely falls inside a repo's path span
/// (the APFS case-fold collision that made `~/Duetto/cto` resolve to the enclosing
/// `hotstats/hotstats-knowledgebase` repo). The tracked-tree check does.
/// Separately, `git rev-parse --show-toplevel` walks *past* a `cwd/.git` that is
/// present but transiently invalid (e.g. a partially-reconstructed gitdir left by
/// a concurrent `git` rescue) all the way up to an ancestor repo — observed live
/// as `~/Duetto/cto` (its own repo) being reported as "inside another repository's
/// working tree (`~/Duetto`)". A directory that physically contains a `.git` entry
/// IS a working-tree root; its identity is its OWN repo, never an ancestor's.
/// What: (0) OWN-REPO-WINS — if `cwd` directly contains a `.git` entry (a
/// directory for a normal repo, or a worktree/submodule gitlink *file*; matched by
/// EXACT name via [`cwd_owns_git_entry`], never a `.git`-prefixed sibling such as
/// `.git.backup`), classify `Usable(cwd)` immediately without consulting git's
/// upward walk. Otherwise (1) resolve the git root; when the root IS `cwd` it is
/// trusted unconditionally; when the root is a strict ancestor, `cwd`'s relative
/// path is checked for tracked-tree membership via [`cwd_is_tracked_within`] — a
/// hit is `Usable`, a miss is `UntrackedInsideAncestor`. A prefix mismatch (e.g.
/// symlink canonicalisation differences) conservatively trusts the root
/// (`Usable`) rather than newly refusing a previously-working checkout.
/// Test: `guided_classify_cwd_own_git_wins_over_ancestor` (#2542 differential),
/// `guided_classify_cwd_own_git_ignores_dotgit_prefixed_sibling` (#2542),
/// `guided_classify_cwd_own_git_worktree_pointer_file` (#2542 gitlink file),
/// `guided_classify_cwd_usable_for_tracked_subdir`,
/// `guided_classify_cwd_untracked_for_uncommitted_subdir`,
/// `guided_classify_cwd_usable_for_repo_root`.
pub(crate) fn classify_cwd_project(cwd: &std::path::Path) -> CwdProject {
    // (0) Own-repo-wins (#2542): a `.git` entry directly in `cwd` means `cwd` is a
    // working-tree root — resolve to its OWN repo and never let git's upward walk
    // adopt an enclosing ancestor. This is deterministic and independent of
    // whether `cwd/.git` is currently a *valid* gitdir (it may be mid-rescue),
    // which is exactly the transient that made the walk skip past it.
    if cwd_owns_git_entry(cwd) {
        return CwdProject::Usable(cwd.to_path_buf());
    }
    let Some(git_root) = find_git_root(cwd) else {
        return CwdProject::NotGit;
    };
    // `git rev-parse --show-toplevel` canonicalises symlinks (e.g. macOS
    // /var → /private/var); canonicalise `cwd` the same way so the prefix
    // comparison is apples-to-apples and a strict subdir is not misread as a
    // prefix mismatch. Both fall back to the raw path if canonicalisation fails.
    let cwd_canon = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let root_canon = git_root.canonicalize().unwrap_or_else(|_| git_root.clone());
    match cwd_canon.strip_prefix(&root_canon) {
        // `cwd` IS the repo root (empty relpath): the normal top-level case —
        // trust it unconditionally, behavior unchanged.
        Ok(rel) if rel.as_os_str().is_empty() => CwdProject::Usable(git_root),
        // Strict subdirectory: only trust the ancestor when `cwd` is tracked.
        Ok(rel) => {
            if cwd_is_tracked_within(&git_root, rel) {
                CwdProject::Usable(git_root)
            } else {
                CwdProject::UntrackedInsideAncestor(git_root)
            }
        }
        // Prefix mismatch (symlink canonicalisation, etc.): cannot compute a
        // relpath — preserve the pre-existing trust-the-root behavior.
        Err(_) => CwdProject::Usable(git_root),
    }
}

/// Return the git root for `cwd` only when `cwd` genuinely belongs to that repo.
///
/// Why: thin adapter so the `cwd → project` derivation paths that only care about
/// the *usable* root (e.g. [`derive_project`]) need not match the full
/// [`CwdProject`] enum. Both an untracked-ancestor `cwd` and a non-git `cwd`
/// collapse to `None` — neither should inherit a git identity.
/// What: returns `Some(root)` for [`CwdProject::Usable`], `None` otherwise.
/// Test: covered transitively via `guided_derive_project_returns_none_for_untracked_ancestor_subdir`
/// and the `classify_cwd_project` unit tests.
fn usable_git_root(cwd: &std::path::Path) -> Option<std::path::PathBuf> {
    match classify_cwd_project(cwd) {
        CwdProject::Usable(root) => Some(root),
        CwdProject::UntrackedInsideAncestor(_) | CwdProject::NotGit => None,
    }
}

/// Is `relpath` (a strict subdirectory of `git_root`) part of the repo's
/// tracked tree?
///
/// Why: the authoritative test for the ancestor-trust guard (#2534). Git's
/// index/tree is case-SENSITIVE, so on a case-insensitive filesystem an untracked
/// `cto` directory that shadows a tracked `CTO` still produces NO tree entry for
/// `cto` — exactly the discriminator needed to reject the case-fold collision.
/// What: shells `git -C <git_root> ls-tree -d --name-only HEAD -- <relpath>` and
/// delegates the yes/no decision to the pure [`ls_tree_reports_tracked_dir`].
/// Fails CLOSED (returns `false`) when git errors or the repo has no `HEAD`
/// (e.g. a commit-less repo has no tracked tree to belong to), so an
/// unverifiable ancestor is never trusted.
/// Test: decision logic is unit-tested via [`ls_tree_reports_tracked_dir`]; the
/// live-git path is exercised by `guided_classify_cwd_*` integration tests.
fn cwd_is_tracked_within(git_root: &std::path::Path, relpath: &std::path::Path) -> bool {
    let rel = relpath.to_string_lossy();
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(git_root)
        .args(["ls-tree", "-d", "--name-only", "HEAD", "--"])
        .arg(rel.as_ref())
        .output();
    match output {
        Ok(o) if o.status.success() => {
            ls_tree_reports_tracked_dir(&String::from_utf8_lossy(&o.stdout))
        }
        _ => false,
    }
}

/// Pure predicate: does `git ls-tree -d --name-only HEAD -- <relpath>` stdout
/// indicate the queried path is a tracked directory?
///
/// Why: isolates the decision from the git shell-out so the tracked-vs-untracked
/// rule is exhaustively unit-testable without a live repo (mirrors the
/// `is_github_remote` pure-helper pattern). `ls-tree` emits one line naming the
/// path only when a matching tracked tree object exists; it emits nothing for an
/// untracked (or case-mismatched) path.
/// What: returns `true` when any output line is non-empty after trimming, `false`
/// otherwise. Empty stdout ⇒ untracked ⇒ `false`.
/// Test: `guided_ls_tree_reports_tracked_dir_true_for_named_entry`,
/// `guided_ls_tree_reports_tracked_dir_false_for_empty`,
/// `guided_ls_tree_reports_tracked_dir_false_for_blank_lines`.
pub(crate) fn ls_tree_reports_tracked_dir(ls_tree_stdout: &str) -> bool {
    ls_tree_stdout.lines().any(|line| !line.trim().is_empty())
}

/// Build the operator-facing notice for the untracked-ancestor case (#2534).
///
/// Why: when `cwd` sits inside another repository's working tree but is not part
/// of it, silently launching that ancestor repo's project is the bug we are
/// fixing. The operator needs to know WHY `tm` declined to adopt the enclosing
/// repo — extracting the wording into a pure helper keeps it unit-testable.
/// What: returns a two-line stderr notice naming the enclosing `git_root` as the
/// reason `tm` is not launching that project, and reassuring the operator that
/// nothing was written.
/// Test: `guided_untracked_ancestor_message_names_root`,
/// `guided_untracked_ancestor_message_does_not_claim_launch`.
pub(crate) fn untracked_ancestor_message(git_root: &std::path::Path) -> String {
    format!(
        "tm: this directory is inside another repository's working tree ({}) \
         but is not part of it — not launching that project.\n\
         tm: nothing was touched. (Use an explicit `tm` subcommand, or run `tm` \
         from a tracked directory of the repo you intend to work in.)",
        git_root.display()
    )
}

/// Daemon-unreachable fallback that protects ALL live git checkouts (#1724).
///
/// Why: the guided default MUST NOT deploy framework files into ANY git
/// project's live checkout, even when the daemon is down. The invariant is:
/// if cwd is inside a git working tree (at ANY depth), it is never written to.
/// What: three-way dispatch on cwd:
///   (1) Inside a GitHub-backed git working tree → delegate to
///       [`redirect_to_managed_clone`] which provisions (or reuses) the protected
///       base clone and per-session worktree, then calls `launch()` against THAT
///       workspace, never the live checkout. Uses the repo ROOT (not cwd) to
///       read the origin remote — correct even from subdirectories.
///   (2) Inside a non-GitHub git working tree (non-GitHub remote or no remote)
///       → return an actionable `Err`; the live checkout is never touched.
///   (3) Not inside any git working tree (plain directory or bare repo) →
///       classic `tm launch` path; there is no live checkout to protect,
///       consistent with the daemon's local-path fast path.
/// Test: `guided_fallback_never_pollutes_github_git_checkout`,
/// `guided_fallback_blocks_non_github_git_checkout`,
/// `guided_fallback_blocks_github_git_from_subdirectory`,
/// `guided_fallback_untracked_ancestor_does_not_redirect`, and
/// `guided_fallback_redirect_success_worktree_not_live_checkout` in
/// `tests_behavior_b_tests.rs`.
pub(crate) async fn fallback_protected(
    client: &reqwest::Client,
    url: &str,
    cwd: &std::path::Path,
) -> anyhow::Result<()> {
    let git_root = match classify_cwd_project(cwd) {
        CwdProject::Usable(root) => root,
        CwdProject::UntrackedInsideAncestor(root) => {
            // cwd is inside another repo's working tree but is NOT part of it
            // (#2534). Do NOT inherit that ancestor's identity — neither the
            // managed-clone redirect nor the non-GitHub refusal is appropriate,
            // because this directory is not that project. Route to the same
            // clean, non-fatal exit as a plain non-git directory.
            eprintln!("{}", untracked_ancestor_message(&root));
            return Ok(());
        }
        CwdProject::NotGit => {
            // Not inside a git working tree: print a helpful note and exit cleanly (#1839).
            eprintln!("{}", super::misc::NON_GIT_FALLBACK_HINT);
            return Ok(());
        }
    };

    // cwd is inside a git working tree it genuinely belongs to — protect it.
    // Read the origin remote from the REPO ROOT so subdirectory calls
    // find the same remote as top-level calls would.
    //
    // NOTE: `parse_github_path` accepts *any* remote URL (not just GitHub);
    // we therefore gate on `is_github_remote` (github.com substring) to
    // distinguish GitHub from Gitea/GitLab/bare-SSH remotes.
    let origin_url = trusty_mpm::daemon::managed_routes::inproject::get_origin_url(&git_root);

    if let Some(ref raw_url) = origin_url
        && is_github_remote(raw_url)
    {
        // GitHub project: redirect deploy to the protected managed clone.
        return redirect_to_managed_clone(client, url, cwd, raw_url).await;
    }

    // Non-GitHub remote or no remote: refuse to write to the live tree.
    // NOTE: the daemon's reachability is irrelevant here — this branch is
    // reached even when the daemon is UP. The real reason for refusal is
    // that `tm` only auto-manages GitHub repositories (#1777).
    let remote_desc = origin_url.as_deref().unwrap_or("(no remote configured)");
    eprintln!("{}", non_github_refusal_message(remote_desc));
    anyhow::bail!(
        "not auto-managing: live git checkout at '{}' is a non-GitHub repository — \
         auto-managed clones require a GitHub remote; \
         use an explicit `tm` subcommand to work here manually",
        git_root.display()
    );
}

/// Redirect the guided-default fallback to the protected managed-clone workspace.
///
/// Why: when the daemon is unreachable and the current directory is a GitHub-backed
/// git project, framework files must go into the managed-clone workspace
/// (`~/trusty-mpm-projects/<owner>/<repo>/.worktrees/<session-id>/`), never
/// into the operator's live checkout (#1724, #1803).
/// What: (1) parses `owner/repo` from `origin_url`; (2) ensures the protected
/// base clone exists (`ensure_base_clone` is idempotent — returns immediately when
/// the clone already exists); (3) creates a per-session git worktree inside the
/// base clone; (4) calls `launch()` with the worktree path as the target directory.
/// If any step fails (unparseable URL, clone error, worktree error), the function
/// returns `Err` with an actionable message — the live checkout is never touched.
/// Test: `guided_fallback_never_pollutes_github_git_checkout`.
async fn redirect_to_managed_clone(
    client: &reqwest::Client,
    url: &str,
    cwd: &std::path::Path,
    origin_url: &str,
) -> anyhow::Result<()> {
    use trusty_mpm::daemon::managed_routes::inproject;
    use trusty_mpm::session_manager::ManagedSessionId;

    // Parse owner/repo from the GitHub remote URL.
    let Some(gh) = trusty_common::github_path::parse_github_path(origin_url) else {
        eprintln!(
            "tm: cannot determine GitHub project from remote URL '{origin_url}'.\n\
             Start the daemon first with `tm start`, then run `tm` again."
        );
        anyhow::bail!(
            "daemon unreachable: cannot parse GitHub remote URL as owner/repo — run `tm start` first"
        );
    };

    // Ensure the protected base clone exists. Idempotent: returns Ok immediately
    // when the clone is already present; clones once on first invocation.
    let base = inproject::base_clone_path(&gh.owner, &gh.repo);
    eprintln!(
        "tm: daemon unreachable — redirecting to protected managed clone\n\
         tm: base clone: {}",
        base.display()
    );
    if let Err(e) = inproject::ensure_base_clone(origin_url, &base) {
        eprintln!(
            "tm: could not set up base clone for {}/{}: {e}\n\
             Start the daemon first with `tm start`, then run `tm` again.",
            gh.owner, gh.repo
        );
        anyhow::bail!("failed to set up managed base clone: {e}");
    }

    // Create a per-session worktree branched from the base clone.
    let session_id = ManagedSessionId::new();
    // NOTE (#2032): this daemon-unreachable fallback path is NOT the managed
    // SessionManager spawn flow — it has no session manager to resolve a
    // semantic tmux name from, so it deliberately keeps the pre-#2032
    // UUID-named worktree here. Only the daemon's `spawn_managed_inproject`
    // path (`daemon::managed_routes::lifecycle`) uses the new semantic-name
    // worktree layout.
    let worktree =
        match inproject::create_session_worktree(&base, &session_id.to_string(), &session_id) {
            Ok(p) => p,
            Err(e) => {
                eprintln!(
                    "tm: could not create per-session worktree: {e}\n\
                 Start the daemon first with `tm start`, then run `tm` again."
                );
                anyhow::bail!("failed to create session worktree: {e}");
            }
        };

    eprintln!(
        "tm: launching in protected workspace (live checkout at {} is untouched)\n\
         tm: session worktree: {}",
        cwd.display(),
        worktree.display()
    );

    // Launch in the session worktree — not the live checkout.
    let dir = worktree.to_string_lossy().to_string();
    super::launch::launch(client, url, Some(dir), None).await
}
