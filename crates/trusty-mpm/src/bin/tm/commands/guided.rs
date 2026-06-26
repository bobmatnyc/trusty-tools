//! Bare `tm` guided default mode (#1708).
//!
//! Why: typing `tm` alone — with no subcommand — should do the most useful
//! thing for the operator's current context rather than printing help and
//! exiting. Inside a git repository with a GitHub remote, the daemon's
//! in-project spawn path (#1706) is the right action: it either reconnects to
//! an existing session for the same project (#1707) or creates a fresh
//! per-session worktree, then attaches the terminal to it. Outside a git repo
//! entirely (no `.git` directory), falling back to `tm launch` (the full
//! framework-deploy path) preserves existing behaviour for non-git directories.
//! Any git working tree — GitHub-backed or not — is protected from in-place
//! framework-file deployment (#1724).
//!
//! What: [`run_guided_default`] detects the current directory, queries the
//! managed spawn endpoint, and then attaches the terminal to the resulting
//! tmux session. A reconnect is fully transparent — the operator sees the same
//! `tmux attach-session -t <name>` outcome whether the session is new or reused.
//!
//! Test: the happy-path and fallback branches are unit-tested in
//! `tests_behavior_a.rs`; the daemon interaction is exercised via the managed
//! spawn integration tests.

use anyhow::Context as _;
use serde::Deserialize;

/// Response shape for `POST /api/v1/sessions/managed` (subset we need).
#[derive(Debug, Deserialize)]
struct SpawnManagedResponse {
    #[serde(default)]
    name: String,
    #[serde(default)]
    state: String,
}

/// Bare `tm` guided default — detect context and spawn or reconnect (#1708).
///
/// Why: operators invoke `tm` without a subcommand from inside a project
/// directory; the guided default eliminates the need to remember whether to
/// type `tm launch`, `tm connect`, `tm sessions new`, or a managed-route
/// command — the daemon decides based on the directory's git identity.
/// What: (1) resolves the current working directory; (2) if the daemon is
/// reachable, posts `POST /api/v1/sessions/managed` with `repo_url = <cwd>` —
/// the daemon's in-project spawn path handles reconnect (Active session for the
/// same `owner/repo` is returned immediately) and new-session creation
/// (a per-session worktree is provisioned); (3) attaches to the resulting tmux
/// session name. When the daemon is unreachable or the spawn fails, delegates
/// to [`fallback_protected`] which preserves the live-checkout guarantee (#1724).
/// Test: `guided_fallback_never_pollutes_github_git_checkout` in
/// `tests_behavior_b_tests.rs` (verifies no framework files in live checkout);
/// `guided_default_attaches_on_success` (mocked daemon response).
pub(crate) async fn run_guided_default(client: &reqwest::Client, url: &str) -> anyhow::Result<()> {
    // 1. Resolve the current directory.
    let cwd = std::env::current_dir().context("cannot resolve current directory")?;
    let workdir = cwd.to_string_lossy().to_string();

    eprintln!("tm: no subcommand — using guided default for {workdir}");

    // 2. Try the managed spawn/reconnect via the daemon.
    //    The daemon's in-project spawn path handles all cases:
    //    - Git repo with GitHub remote → reconnect or new worktree session.
    //    - Non-git dir or no GitHub remote → local-path fast path.
    //    We keep the task and git_ref minimal so the daemon picks sensible defaults.
    let resp = client
        .post(format!("{url}/api/v1/sessions/managed"))
        .json(&serde_json::json!({
            "repo_url": workdir,
            "ref": "HEAD",
            "task": "",
        }))
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => match r.json::<SpawnManagedResponse>().await {
            Ok(body) if !body.name.is_empty() => {
                eprintln!("tm: attaching to session '{}' ({})", body.name, body.state);
                let status = std::process::Command::new("tmux")
                    .args(["attach-session", "-t", &body.name])
                    .status()
                    .context("failed to invoke tmux")?;
                if !status.success() {
                    anyhow::bail!("tmux attach-session exited with failure");
                }
                return Ok(());
            }
            Ok(_) => {
                eprintln!("tm: daemon returned empty session name; falling back");
            }
            Err(e) => {
                eprintln!("tm: failed to parse daemon response ({e}); falling back");
            }
        },
        Ok(r) => {
            eprintln!(
                "tm: daemon returned {} for managed spawn; falling back",
                r.status()
            );
        }
        Err(e) => {
            eprintln!("tm: daemon unreachable ({e}); falling back");
        }
    }

    // 3. Daemon unreachable or spawn failed — delegate to the protected fallback.
    //    This NEVER deploys framework files into a GitHub-backed live checkout
    //    (#1724): GitHub projects are redirected to their managed-clone workspace;
    //    non-git directories retain the classic `tm launch` behaviour.
    fallback_protected(client, url, &cwd).await
}

/// Return true when the remote URL targets github.com (any transport).
///
/// Why: `parse_github_path` from `trusty-common` accepts *any* git remote URL,
/// not just GitHub ones. We need a host-specific guard so that non-GitHub git
/// projects (Gitea, GitLab, bare SSH, etc.) are refused rather than having a
/// clone attempt made against an unrecognised host (#1724 residual gap).
/// What: case-insensitive substring match for `"github.com"` in the raw URL —
/// catches both HTTPS (`https://github.com/…`) and SSH (`git@github.com:…`)
/// forms without adding a URL-parsing dependency.
/// Test: `guided_fallback_blocks_non_github_git_checkout` verifies a Gitea URL
/// is NOT treated as GitHub; `guided_fallback_never_pollutes_github_git_checkout`
/// verifies a real GitHub URL is recognised.
fn is_github_remote(url: &str) -> bool {
    url.to_ascii_lowercase().contains("github.com")
}

/// Daemon-unreachable fallback that protects ALL live git checkouts (#1724).
///
/// Why: the guided default MUST NOT deploy framework files into ANY git
/// project's live checkout, even when the daemon is down. The invariant is:
/// if `.git` is detected, the live working tree is never written to.
/// What: three-way dispatch on cwd:
///   (1) GitHub-backed git project → delegate to [`redirect_to_managed_clone`]
///       which provisions (or reuses) the protected base clone and per-session
///       worktree, then calls `launch()` against THAT workspace, never the live
///       checkout.
///   (2) Non-GitHub git project (non-GitHub remote or no remote) → return an
///       actionable `Err`; the live checkout is never touched.
///   (3) Non-git directory (no `.git`) → classic `tm launch` path; there is no
///       working tree to protect, consistent with the daemon's local-path fast
///       path for plain directories.
/// Test: `guided_fallback_never_pollutes_github_git_checkout` and
/// `guided_fallback_blocks_non_github_git_checkout` in `tests_behavior_b_tests.rs`.
pub(crate) async fn fallback_protected(
    client: &reqwest::Client,
    url: &str,
    cwd: &std::path::Path,
) -> anyhow::Result<()> {
    if cwd.join(".git").exists() {
        // Any git working tree is protected from in-place framework-file
        // deployment (#1724). Dispatch on remote type:
        //   • GitHub remote (github.com host) → redirect to managed clone.
        //   • Non-GitHub remote or no remote   → actionable Err; never deploy.
        //
        // NOTE: `parse_github_path` accepts *any* remote URL (not just GitHub);
        // we therefore check for "github.com" in the URL explicitly rather than
        // relying on a successful parse alone.
        let origin_url = trusty_mpm::daemon::managed_routes::inproject::get_origin_url(cwd);

        if let Some(ref raw_url) = origin_url
            && is_github_remote(raw_url)
        {
            // GitHub project: redirect deploy to the protected managed clone.
            return redirect_to_managed_clone(client, url, cwd, raw_url).await;
        }

        // Non-GitHub remote or no remote at all: refuse to write to the live tree.
        let remote_desc = origin_url.as_deref().unwrap_or("(no remote configured)");
        eprintln!(
            "tm: daemon unreachable — refusing to deploy into live git checkout.\n\
             tm: Auto-protected managed clones require a GitHub remote \
             (detected remote: {remote_desc}).\n\
             tm: Start the daemon with `tm start`, then run `tm` again."
        );
        anyhow::bail!(
            "daemon unreachable: live git checkout at '{}' is protected — \
             auto-managed clones require a GitHub remote; \
             start the daemon with `tm start` first",
            cwd.display()
        );
    }

    // Non-git directory: classic tm launch path (no working tree to protect).
    super::launch::launch(client, url, None, None).await
}

/// Redirect the guided-default fallback to the protected managed-clone workspace.
///
/// Why: when the daemon is unreachable and the current directory is a GitHub-backed
/// git project, framework files must go into the managed-clone workspace
/// (`~/trusty-tools/repos/<owner>/<repo>/worktrees/<session-id>/`), never
/// into the operator's live checkout (#1724).
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
    let worktree = match inproject::create_session_worktree(&base, &session_id) {
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
