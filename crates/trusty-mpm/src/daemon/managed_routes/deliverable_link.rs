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
//! bare `repo_url` via [`crate::project::resolver::resolve_project_by_repo_url`],
//! then reuses [`crate::daemon::api::deliverable_routes::fetch_scoped`] — the
//! EXACT existence+project-scope check the Deliverable CRUD routes already
//! enforce — so this gate can never drift from what "belongs to this
//! project" means there. A 404-style [`DaemonError`] renders identically
//! whether it originated here or from the CRUD routes.
//! Test: `validate_deliverable_scope_unknown_project_is_404`,
//! `validate_deliverable_scope_unknown_deliverable_is_404`,
//! `validate_deliverable_scope_wrong_project_is_404`,
//! `validate_deliverable_scope_valid_id_passes` in this module's own `tests`.

use std::sync::Arc;

use axum::response::{IntoResponse, Response};

use crate::daemon::api::deliverable_routes::fetch_scoped;
use crate::daemon::error::DaemonError;
use crate::daemon::state::DaemonState;
use crate::project::resolver::resolve_project_by_repo_url;

/// Validate that `deliverable_id` exists and belongs to the project that owns
/// `repo_url`, rendering a ready-to-return [`Response`] on failure.
///
/// Why: `spawn_session` must reject an invalid `--deliverable` BEFORE calling
/// `spawn_managed` (mirroring the existing runtime-selector 400 pre-check) so
/// a typo'd or cross-project id never mints workspace/tmux infrastructure for
/// a link that was never going to be recorded (§11: this is a pointer check,
/// not a Deliverable mutation — nothing here writes to the Deliverable store).
/// What: (1) lists registered projects and resolves one whose `repo_url`
/// matches; no match → 404 (there is no project to scope the id against).
/// (2) delegates to [`fetch_scoped`] for the existence+scope check, mapping
/// any [`DaemonError`] straight to its response.
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
    let Some(project) = resolve_project_by_repo_url(repo_url, &projects) else {
        return Err(DaemonError::DeliverableNotFound {
            id: deliverable_id.to_string(),
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
