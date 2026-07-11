//! Pre-spawn `--deliverable` validation gate (DOC-35 §10.6, #2379).
//!
//! Why: `managed_routes/mod.rs` was near its 500-SLOC production cap; this
//! validation is also a single, cohesive concern (mirroring why `summary.rs`
//! and `mcp_spawn_gate.rs` already live as siblings rather than inline in
//! `mod.rs`). The check itself must run BEFORE any provisioning side effect —
//! the same "fail closed before touching disk" shape the MCP spawn gate
//! already uses — so an invalid `--deliverable <id>` never leaves an orphan
//! workspace or tmux session behind.
//! What: [`validate_deliverable_scope`] resolves the spawning project from a
//! bare `repo_url` via [`resolve_project_for_deliverable_scope`], then reuses
//! [`crate::daemon::api::deliverable_routes::fetch_scoped`] — the EXACT
//! existence+project-scope check the Deliverable CRUD routes already
//! enforce — so this gate can never drift from what "belongs to this
//! project" means there. A 404-style [`DaemonError`] renders identically
//! whether it originated here or from the CRUD routes.
//!
//! Project resolution mirrors the identity the in-project spawn path already
//! derives for `source_id` (`daemon::managed_routes::lifecycle`,
//! `format!("{owner}/{repo}")`) rather than a bare string compare of
//! `repo_url` against `Project::repo_url` (review HIGH, #2379): a LOCAL-PATH
//! spawn's `repo_url` is a filesystem path, never a registered project's URL
//! string, and an SSH-form `repo_url` never string-matches an
//! HTTPS-registered project (or vice versa) even though both name the same
//! GitHub repo. [`resolve_project_for_deliverable_scope`] parses BOTH sides
//! down to a slugified `{owner, repo}`
//! ([`trusty_common::github_path::parse_github_path`]) before comparing, and
//! for a local directory reads its actual git remote first — exactly what
//! [`super::inproject::try_inproject_spawn`] already does. It falls back to
//! [`crate::project::resolver::resolve_project_by_repo_url`]'s exact-string
//! match only when no GitHub identity is derivable from either side (a
//! non-GitHub-hosted project, or a local directory with no git remote).
//!
//! TOCTOU note: this validates the Deliverable exists and is in-scope, then
//! the spawn proceeds, and only AFTER the session record is created does
//! [`crate::session_manager::SessionManager::set_deliverable_id`] persist the
//! pointer — if the Deliverable is deleted in that window the persisted link
//! becomes a stale pointer to a since-deleted id. This is accepted under the
//! pointer-only contract (§11): the link is bookkeeping, not a live
//! foreign-key relationship the daemon enforces continuously, and a stale
//! pointer is no different in kind from decommissioning a session without
//! ever unlinking it (§10.6) — both are inert history, not correctness bugs.
//! Test: `validate_deliverable_scope_unknown_project_is_404`,
//! `validate_deliverable_scope_unknown_deliverable_is_404`,
//! `validate_deliverable_scope_wrong_project_is_404`,
//! `validate_deliverable_scope_valid_id_passes`,
//! `validate_deliverable_scope_local_path_matches_registered_project`,
//! `validate_deliverable_scope_ssh_repo_url_matches_https_registered_project`
//! in this module's own `tests`.

use std::sync::Arc;

use axum::response::{IntoResponse, Response};
use trusty_common::github_path::{GithubPath, parse_github_path};

use crate::daemon::api::deliverable_routes::fetch_scoped;
use crate::daemon::error::DaemonError;
use crate::daemon::state::DaemonState;
use crate::project::record::Project;
use crate::project::resolver::resolve_project_by_repo_url;

/// Derive the slugified GitHub `{owner, repo}` identity of a spawn's
/// `repo_url`, resolving a LOCAL-PATH `repo_url` via its actual git remote.
///
/// Why: a local-path spawn's `repo_url` (#1433/#1706 — the dominant
/// in-project path) is a filesystem path, not a URL; deriving its identity
/// requires reading `remote.origin.url` first, exactly like
/// [`super::inproject::try_inproject_spawn`] does. `parse_github_path` alone
/// would happily (and WRONGLY) mis-derive an `{owner, repo}` pair from a bare
/// path's trailing segments, so the local-directory branch must run FIRST.
/// What: `is_local_workdir` → read `remote.origin.url` via
/// `inproject::get_origin_url`, then parse that; otherwise parse `repo_url`
/// directly (the remote-URL, HTTPS-or-SSH case). Either way the result is
/// normalised via [`trusty_common::github_path::parse_github_path`], which
/// already treats SSH and HTTPS forms identically.
/// Test: covered transitively by `validate_deliverable_scope_local_path_*`
/// and `validate_deliverable_scope_ssh_repo_url_*`.
fn github_identity_for_repo_url(repo_url: &str) -> Option<GithubPath> {
    if super::is_local_workdir(repo_url) {
        let origin = super::inproject::get_origin_url(std::path::Path::new(repo_url))?;
        parse_github_path(&origin)
    } else {
        parse_github_path(repo_url)
    }
}

/// Resolve the registered [`Project`] that owns a spawn's `repo_url`, for
/// Deliverable-linkage validation (DOC-35 §10.6, #2379 review HIGH).
///
/// Why: see the module doc's "Project resolution" section — a bare
/// `repo_url`-string compare (the pre-review implementation) 404s a valid
/// `--deliverable` for the two dominant real-world mismatches: a local-path
/// spawn, and an SSH-vs-HTTPS form mismatch.
/// What: derives `repo_url`'s `{owner, repo}` via
/// [`github_identity_for_repo_url`] and looks for a registered project whose
/// OWN `repo_url` parses to the SAME `{owner, repo}` (each project's
/// `repo_url` is parsed the same way `parse_github_path` always is — no
/// local-path branch needed there since a registered `Project::repo_url` is
/// always a remote URL, never a filesystem path). Falls back to
/// [`resolve_project_by_repo_url`]'s exact-string match when either side has
/// no parseable GitHub identity, so a project registered under a literal,
/// non-GitHub URL (or a local repo_url with no git remote at all) still
/// resolves exactly as before.
/// Test: see module doc.
fn resolve_project_for_deliverable_scope<'a>(
    repo_url: &str,
    projects: &'a [Project],
) -> Option<&'a Project> {
    if let Some(target) = github_identity_for_repo_url(repo_url)
        && let Some(project) = projects
            .iter()
            .find(|p| parse_github_path(&p.repo_url).as_ref() == Some(&target))
    {
        return Some(project);
    }
    resolve_project_by_repo_url(repo_url, projects)
}

/// Validate that `deliverable_id` exists and belongs to the project that owns
/// `repo_url`, rendering a ready-to-return [`Response`] on failure.
///
/// Why: `spawn_session` must reject an invalid `--deliverable` BEFORE calling
/// `spawn_managed` (mirroring the existing runtime-selector 400 pre-check) so
/// a typo'd or cross-project id never mints workspace/tmux infrastructure for
/// a link that was never going to be recorded (§11: this is a pointer check,
/// not a Deliverable mutation — nothing here writes to the Deliverable store).
/// What: (1) lists registered projects and resolves one that owns `repo_url`
/// via [`resolve_project_for_deliverable_scope`]; no match →
/// [`DaemonError::ProjectNotFoundForRepoUrl`] (distinct from a Deliverable
/// 404 — the deliverable itself may well exist, there is simply no project
/// to scope it against). (2) delegates to [`fetch_scoped`] for the
/// existence+scope check, mapping any [`DaemonError`] straight to its
/// response.
/// Test: see module doc.
pub(super) async fn validate_deliverable_scope(
    state: &Arc<DaemonState>,
    repo_url: &str,
    deliverable_id: &str,
) -> Result<(), Response> {
    let projects = state
        .project_registry()
        .await
        .list()
        .await
        .unwrap_or_default();
    let Some(project) = resolve_project_for_deliverable_scope(repo_url, &projects) else {
        return Err(DaemonError::ProjectNotFoundForRepoUrl {
            repo_url: repo_url.to_string(),
        }
        .into_response());
    };
    fetch_scoped(state, &project.name, deliverable_id)
        .await
        .map(|_| ())
        .map_err(IntoResponse::into_response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use chrono::Utc;
    use tempfile::TempDir;

    use crate::deliverable::{
        Deliverable, DeliverableId, DeliverableKind, DeliverableStatus, EstimationTier,
    };
    use crate::project::record::Project;

    fn state() -> (Arc<DaemonState>, TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let st = Arc::new(DaemonState::with_root(dir.path().to_path_buf()));
        (st, dir)
    }

    fn project(name: &str, repo_url: &str) -> Project {
        Project {
            name: name.to_string(),
            repo_url: repo_url.to_string(),
            default_branch: "main".to_string(),
            stack_hint: None,
            tags: vec![],
            description: None,
            gh_user: None,
            github: None,
            commit_name: None,
            commit_email: None,
        }
    }

    fn deliverable(project_name: &str) -> Deliverable {
        Deliverable {
            id: DeliverableId::new(),
            project_name: project_name.to_string(),
            name: "sample".into(),
            description: String::new(),
            kind: DeliverableKind::Feature,
            ticket_ref: None,
            spec_ref: None,
            status: DeliverableStatus::Proposed,
            estimated_effort: EstimationTier::M,
            created_at: Utc::now(),
            target_date: None,
        }
    }

    /// Initialise a temp git repo with a `remote.origin.url` and return its path.
    ///
    /// Why: exercising the LOCAL-PATH branch of
    /// [`github_identity_for_repo_url`] needs a real directory `git` will
    /// treat as a repo with an origin — mirrors
    /// `get_origin_url_returns_some_for_git_repo_with_origin` in
    /// `tests/local_spawn.rs`.
    /// What: `git init` then `git remote add origin <origin_url>`.
    fn git_repo_with_origin(dir: &std::path::Path, origin_url: &str) {
        let init = std::process::Command::new("git")
            .args(["init", dir.to_str().expect("path is utf8")])
            .output()
            .expect("git init");
        assert!(init.status.success(), "git init failed");
        let remote = std::process::Command::new("git")
            .args([
                "-C",
                dir.to_str().expect("path is utf8"),
                "remote",
                "add",
                "origin",
                origin_url,
            ])
            .output()
            .expect("git remote add");
        assert!(remote.status.success(), "git remote add failed");
    }

    #[tokio::test]
    async fn validate_deliverable_scope_unknown_project_is_404() {
        let (st, _g) = state();
        // No project registered for this repo_url at all.
        let err = validate_deliverable_scope(&st, "https://github.com/org/unregistered", "any-id")
            .await
            .expect_err("no project to scope against");
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn validate_deliverable_scope_local_path_matches_registered_project() {
        // #2379 review HIGH: a local-path spawn's `repo_url` is a filesystem
        // path, never a registered project's URL string — resolution must
        // read the directory's actual git remote and match by {owner, repo},
        // not by exact `repo_url` string equality.
        let (st, _g) = state();
        st.project_registry()
            .await
            .register(project(
                "trusty-tools",
                "https://github.com/org/trusty-tools",
            ))
            .await
            .expect("register");
        let d = deliverable("trusty-tools");
        st.deliverable_manager()
            .await
            .upsert_deliverable(d.clone())
            .await
            .expect("upsert");

        let repo_dir = tempfile::tempdir().expect("repo tempdir");
        git_repo_with_origin(repo_dir.path(), "https://github.com/org/trusty-tools.git");

        validate_deliverable_scope(
            &st,
            repo_dir.path().to_str().expect("path is utf8"),
            &d.id.to_string(),
        )
        .await
        .expect("local-path repo_url must resolve to the matching registered project");
    }

    #[tokio::test]
    async fn validate_deliverable_scope_ssh_repo_url_matches_https_registered_project() {
        // #2379 review HIGH: `normalise_url`'s exact-string fallback only
        // trims `/`/`.git`; it does not understand SSH-vs-HTTPS equivalence.
        // An SSH-form `repo_url` must still match an HTTPS-registered project
        // naming the SAME GitHub repo.
        let (st, _g) = state();
        st.project_registry()
            .await
            .register(project(
                "trusty-tools",
                "https://github.com/org/trusty-tools",
            ))
            .await
            .expect("register");
        let d = deliverable("trusty-tools");
        st.deliverable_manager()
            .await
            .upsert_deliverable(d.clone())
            .await
            .expect("upsert");

        validate_deliverable_scope(
            &st,
            "git@github.com:org/trusty-tools.git",
            &d.id.to_string(),
        )
        .await
        .expect("SSH-form repo_url must resolve to the HTTPS-registered project");
    }

    #[tokio::test]
    async fn validate_deliverable_scope_unknown_deliverable_is_404() {
        let (st, _g) = state();
        st.project_registry()
            .await
            .register(project(
                "trusty-tools",
                "https://github.com/org/trusty-tools",
            ))
            .await
            .expect("register");

        let err = validate_deliverable_scope(
            &st,
            "https://github.com/org/trusty-tools",
            &DeliverableId::new().to_string(),
        )
        .await
        .expect_err("deliverable does not exist");
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn validate_deliverable_scope_wrong_project_is_404() {
        let (st, _g) = state();
        st.project_registry()
            .await
            .register(project(
                "trusty-tools",
                "https://github.com/org/trusty-tools",
            ))
            .await
            .expect("register");
        // Deliverable exists, but belongs to a DIFFERENT project than the one
        // `repo_url` resolves to.
        let d = deliverable("some-other-project");
        st.deliverable_manager()
            .await
            .upsert_deliverable(d.clone())
            .await
            .expect("upsert");

        let err = validate_deliverable_scope(
            &st,
            "https://github.com/org/trusty-tools",
            &d.id.to_string(),
        )
        .await
        .expect_err("wrong-project deliverable must 404, not leak");
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn validate_deliverable_scope_valid_id_passes() {
        let (st, _g) = state();
        st.project_registry()
            .await
            .register(project(
                "trusty-tools",
                "https://github.com/org/trusty-tools",
            ))
            .await
            .expect("register");
        let d = deliverable("trusty-tools");
        st.deliverable_manager()
            .await
            .upsert_deliverable(d.clone())
            .await
            .expect("upsert");

        validate_deliverable_scope(
            &st,
            "https://github.com/org/trusty-tools",
            &d.id.to_string(),
        )
        .await
        .expect("matching project + existing deliverable must pass");
    }
}
