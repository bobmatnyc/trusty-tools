//! `session start` handler — protected-path routing (#1916).
//!
//! Why: split out of `session.rs` (issue #610 SLOC cap) — the git-repo-detection
//! branch and the preserved in-place fallback are a cohesive, independently
//! testable unit.
//! What: [`start_session`] is the public entry point the `session` dispatcher
//! calls for `SessionAction::Start`; [`start_session_in_place`] is the
//! pre-#1916 in-place deploy-and-start behavior, preserved for directories that
//! are not a recognized git repo.
//! Test: `session_start_dispatches_managed_new_for_github_repo`,
//! `session_start_in_place_writes_stash_and_hard_fails_on_daemon_unreachable`
//! in `start_tests.rs`.

use serde::Deserialize;

use crate::cli::SessionAction;
use crate::commands::project::resolve_dir;
use crate::formatters::session::deploy_summary_line;

/// `session start` — launch a session, routed through protected-path segregation
/// for a recognized GitHub-backed git repo (#1916).
///
/// Why: before this fix, `session start` called `prepare_session` directly on
/// the given/cwd directory and hand-rolled a `POST /sessions` + raw `tmux
/// new-session`/`send-keys`, completely bypassing the protected-path/managed-
/// clone routing `session new` uses (`ManagedNew` → `spawn_managed` →
/// `spawn_managed_inproject`). That meant running `tm session start` inside an
/// ordinary, non-tm-managed git checkout with a remote wrote tm session
/// artifacts (statusline config, deployed agents/skills,
/// `.trusty-mpm/last-instructions.md`) straight into that live checkout — see
/// issue #1916. The confirmed design directive (2026-07-02): "An in-place start
/// should pick up git source info, but it's still running in a protected source
/// tree" — detect project identity from the directory, but launch inside the
/// SAME protected worktree `session new` already provides.
/// What: resolves `dir` (defaulting to cwd) and canonicalizes it. When the
/// resolved directory IS a recognized GitHub-backed git repository — detected
/// via [`crate::commands::guided::derive_project`], the SAME detector the bare
/// `tm` guided default uses (#1916 item 3), so the two surfaces can never
/// diverge on what counts as "protected" — this builds the IDENTICAL
/// [`SessionAction::New`] request `session new`/`ManagedNew` would receive
/// (using the git ROOT as `repo` so a subdirectory invocation still resolves
/// the right project, and an empty `task` for an interactive session, matching
/// the guided default's `launch_new_session_and_attach`) and dispatches it
/// through [`crate::commands::managed_route::run`] — the exact code path
/// `spawn_managed`/`spawn_managed_inproject` serve. When the directory is NOT a
/// recognized git repo (no `.git`, or a git repo with no parseable remote)
/// there is no live source tree to protect: this is the same "just run claude
/// here, no segregation" case `tm connect`'s doc comment documents as
/// supported, so the original in-place `prepare_session` + detached-tmux-start
/// behavior ([`start_session_in_place`]) is preserved unchanged.
/// Test: `session_start_dispatches_managed_new_for_github_repo`,
/// `session_start_in_place_writes_stash_and_hard_fails_on_daemon_unreachable`
/// in `start_tests.rs`.
pub(crate) async fn start_session(
    client: &reqwest::Client,
    url: &str,
    dir: Option<String>,
) -> anyhow::Result<()> {
    let path = resolve_dir(dir)?;
    let path = path.canonicalize().unwrap_or(path);

    if let Some((_source_id, _workspace, git_root)) = crate::commands::guided::derive_project(&path)
    {
        // Protected path: dispatch the IDENTICAL request `session new` builds,
        // through the IDENTICAL chat-core route (`ManagedNew` → `spawn_managed`
        // → `spawn_managed_inproject` for this in-project local-dir case).
        let new_action = SessionAction::New {
            repo: git_root.to_string_lossy().to_string(),
            git_ref: "HEAD".to_string(),
            task: String::new(),
            name_hint: None,
            runtime: trusty_mpm::runtime::RuntimeKind::default(),
            // Empty task → injection is a no-op regardless; keep the turnkey
            // default so `session start` mirrors `session new` semantics (#1903).
            no_inject: false,
            // `session start` has no `--deliverable` surface of its own (#2379).
            deliverable: None,
        };
        crate::commands::managed_route::run(client, url, &new_action).await?;
        return Ok(());
    }

    // #4832: a directory that is not inside ANY git working tree has no project
    // to own its harness state. `prepare_session` would write `.trusty-mpm/`
    // wherever the operator's shell happened to be standing — the observed
    // stray `~/.trusty-mpm/framework/INSTRUCTIONS-COMPILED.md`. Refuse instead.
    // A git repo with no parseable remote still falls through to the in-place
    // path below: `derive_project` returned `None` for the REMOTE, not for the
    // repo, and that project does have a root to write to.
    refuse_outside_a_git_project(&path)?;

    // Not a recognized GitHub-backed remote: no live source tree to protect —
    // preserve the original in-place deploy-and-start behavior.
    let fw = trusty_mpm::core::paths::FrameworkPaths::default();
    start_session_in_place(client, url, &path, &fw).await
}

/// Refuse a launch from a directory that belongs to no git project (#4832).
///
/// Why: harness state belongs to a project, and outside a repository there is
/// no project to attach it to. The pre-#4832 behavior was to deploy anyway,
/// scattering a `.trusty-mpm/` (plus a `CLAUDE.md` stub and a `.claude/` tier)
/// into whatever directory the operator happened to be in. A clear refusal that
/// names the directory and the remedy is strictly better than a launch that
/// silently litters — and this is the routing point that decides it, so the
/// check cannot be bypassed by a caller that forgets it.
/// What: `Ok(())` when [`trusty_mpm::core::harness_root::harness_root_for`]
/// resolves a checkout for `path`; otherwise an error naming the directory and
/// telling the operator to run `git init` or `cd` into a repository. Split out
/// as a named function so the refusal is unit-testable without a daemon.
/// Test: `session_start_refuses_a_non_git_directory`,
/// `session_start_accepts_a_git_directory`.
pub(crate) fn refuse_outside_a_git_project(path: &std::path::Path) -> anyhow::Result<()> {
    if trusty_mpm::core::harness_root::harness_root_for(path).is_some() {
        return Ok(());
    }
    anyhow::bail!(
        "{} is not inside a git repository, so there is no project to hold this \
         session's state.\n\
         `tm` writes a session's agents, instructions and compiled prompt under the \
         project's `.trusty-mpm/`; starting here would scatter them into this \
         directory instead. Run `git init` here, or `cd` into the repository you \
         meant to work in, and retry.",
        path.display()
    )
}

/// The pre-#1916 `session start` behavior: deploy `prepare_session` directly
/// into `path` and start a detached tmux session there.
///
/// Why: extracted verbatim from the original `SessionAction::Start` arm so the
/// non-git / no-remote "just run claude here" case — [`start_session`]'s doc
/// explains why it remains supported — keeps its exact prior behavior with zero
/// regression risk. `fw` is threaded in (rather than resolved internally via
/// `FrameworkPaths::default()`) so tests can supply a hermetic
/// `FrameworkPaths::under(tempdir)` and exercise this function without writing
/// into the developer's/CI's real `~/.claude`/`~/.trusty-mpm` — the real CLI
/// call site in [`start_session`] still passes `FrameworkPaths::default()`, so
/// production behavior is unchanged.
/// What: runs `prepare_session` (deploys agents AND skills — printing both
/// `deploy_summary_line` counts, #1917 — merges CLAUDE.md, prints the
/// catch-up digest), registers via `POST /sessions`, then creates a detached
/// tmux session rooted at `path` and starts `claude` in it.
/// Test: `session_start_in_place_writes_stash_and_hard_fails_on_daemon_unreachable`
/// in `start_tests.rs` covers the routing decision hermetically; the tmux/daemon
/// I/O is exercised by the pre-existing `tests/session_manager_mvp.rs` coverage
/// this function inherited unchanged.
async fn start_session_in_place(
    client: &reqwest::Client,
    url: &str,
    path: &std::path::Path,
    fw: &trusty_mpm::core::paths::FrameworkPaths,
) -> anyhow::Result<()> {
    // Prepare the custom instructions Claude Code reads at startup:
    // deploy composed agents to `~/.claude/agents/` and merge the
    // project CLAUDE.md. This shared prep is what makes a plain
    // `claude` process behave as a trusty-mpm session.
    match trusty_mpm::core::session_launch::prepare_session(fw, path) {
        Ok(report) => {
            println!(
                "{}",
                deploy_summary_line(
                    "Agents",
                    report.deploy.deployed.len(),
                    report.deploy.skipped.len(),
                    report.deploy.unchanged.len(),
                )
            );
            println!(
                "{}",
                deploy_summary_line(
                    "Skills",
                    report.skill_deploy.deployed.len(),
                    report.skill_deploy.skipped.len(),
                    report.skill_deploy.unchanged.len(),
                )
            );
            if report.instructions.claude_md_created {
                println!("  Created CLAUDE.md stub in {}", path.display());
            }
            println!(
                "Instructions: {} agents in delegation authority",
                report.instructions.agent_count
            );
            println!(
                "  Merged instructions written to {}",
                report.stash.display()
            );
            // DOC-28 cutover bridge: print catch-up digest as seed context.
            // CUTOVER BRIDGE — remove post-migration (#1762)
            if let Some(ctx) = report.catchup_context {
                println!("\n---\n\n## Recent Activity (catch-up)\n\n{ctx}");
            }
            // Issue #2149: a roster-deploy failure no longer aborts
            // preparation (the trusty-mpm identity/output-style still gets
            // written), but the operator must see it — not just a
            // suspiciously low deploy count above.
            for err in &report.roster_errors {
                eprintln!("error: roster provisioning gap: {err}");
            }
        }
        // #4752: fatal — refuse to start rather than run a session whose
        // compiled instructions could not be written. Other prep failures stay
        // non-fatal (#2149).
        Err(err) if err.is_fatal() => anyhow::bail!("{err}"),
        Err(err) => eprintln!("warning: session preparation failed: {err}"),
    }

    #[derive(Deserialize)]
    struct Body {
        #[serde(default)]
        name: String,
    }
    let body: Body = client
        .post(format!("{url}/sessions"))
        .json(&serde_json::json!({
            "project": path,
            "project_path": path,
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    // The daemon only registers session state now — it no longer
    // spawns the tmux host (that caused session proliferation). The
    // CLI owns the actual launch: create a detached tmux session in
    // the project directory and start `claude` in it. #2398: routes through
    // `core::tmux::create_managed_session`, the crate's single session-
    // creation choke point, so the configured scrollback/mouse ergonomics
    // are applied before the pane exists.
    let workdir = path.to_string_lossy().to_string();
    let new_session =
        trusty_mpm::core::tmux::create_managed_session(None, &body.name, Some(&workdir));
    match new_session {
        Ok(outcome) if outcome.output.status.success() => {
            if !outcome.options_verified {
                // #3386 review: a caller-visible one-line notice — never
                // silently proceed as if the pane's scrollback limit landed.
                eprintln!(
                    "warning: tmux scrollback limit could not be verified for session {} — the \
                     pane may be capped at tmux's factory 2000-line history-limit instead of \
                     the configured value; see issue #3386",
                    body.name
                );
            }
            // #2997: disclaim the pane's `claude` off the shared tmux server
            // (same wrapper the daemon + `tm launch`/`connect` paths use).
            // No-op off macOS / under TM_DISABLE_SPAWN_DISCLAIM.
            // #4467: the launch line itself is built in the LIBRARY
            // (`model_inject::build_inplace_session_command`) so it carries the
            // shared inherited-marker scrub and is readable by the
            // `transcript_saving` doctor check. It used to be hand-built here as
            // `format!("claude {PERMISSION_MODE_FLAG}")` — a sixth interactive
            // launch line that silently saved no transcript.
            let claude_cmd = trusty_mpm::core::spawn_disclaim::disclaim_pane_command(
                &trusty_mpm::core::model_inject::build_inplace_session_command(),
            );
            let send = trusty_mpm::core::tmux::send_line(
                None,
                &trusty_mpm::core::tmux::TmuxTarget::session(&body.name),
                &claude_cmd,
            )
            .map(|output| output.status);
            match send {
                Ok(s) if s.success() => {
                    println!("started session {} (tmux + claude)", body.name);
                }
                Ok(_) | Err(_) => {
                    eprintln!(
                        "warning: tmux session {} created but failed to start claude",
                        body.name
                    );
                    println!("started session {}", body.name);
                }
            }
        }
        Ok(_) | Err(_) => {
            eprintln!(
                "warning: failed to create tmux session {}; run `claude` manually in {}",
                body.name, workdir
            );
            println!("started session {}", body.name);
        }
    }
    Ok(())
}

// Unit tests live in session/start_tests.rs (test-file budget: 1500 SLOC).
#[cfg(test)]
#[path = "start_tests.rs"]
mod tests;
