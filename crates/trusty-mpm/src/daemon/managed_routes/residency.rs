//! The `mpm.residency.active` route: trusty-mpm as the active-project
//! producer (#7087 slice 1b).
//!
//! Why: slice 1a defined the wire contract
//! ([`trusty_common::residency::ActiveProjectSet`]) and the consumer-side
//! freshness state machine but shipped no producer — nothing yet answers
//! [`trusty_common::mpm_rpc::METHOD_RESIDENCY_ACTIVE`]. This is that producer:
//! it reads the SAME managed-session store every other route reads, so
//! "active" here means exactly what `tm session ls` already shows, not a
//! second derivation that can drift from it.
//! What: [`active_projects_core`] lists every managed session, keeps the ones
//! whose persisted state is `Active` or `Provisioning` AND whose `tmux_name`
//! a live `tmux list-sessions` still recognizes, groups the survivors by
//! project root (`workspace_path` if set, else `cwd`), and derives each
//! group's palace id / trusty-search index id(s) through
//! [`SessionManager::residency_cache_lookup`] /
//! [`SessionManager::residency_cache_store`] so a session whose root has not
//! changed since the last poll never re-touches the filesystem or `git`.
//! **Fail-closed, never fail-empty:** a `tmux list_sessions` error is logged
//! and every persisted `Active`/`Provisioning` record is treated as active
//! rather than the set collapsing to empty — an empty answer would evict
//! every consumer's pinned project at once (#6836), which is exactly what
//! this producer exists to prevent.
//! Test: `residency_tests` (sibling file).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use trusty_common::residency::{ACTIVE_PROJECT_SET_SCHEMA, ActiveProject, ActiveProjectSet};

use crate::daemon::rpc::managed::outcome::RouteOutcome;
use crate::daemon::state::DaemonState;
use crate::session_manager::{ManagedSessionId, ManagedSessionState, SessionRecord};

/// Per-root accumulator while grouping the active sessions.
struct ProjectAccum {
    root: PathBuf,
    repo_url: Option<String>,
    session_ids: Vec<ManagedSessionId>,
    session_id_strings: Vec<String>,
    last_activity_unix: Option<u64>,
}

/// The transport-neutral body of `mpm.residency.active`.
///
/// Test: `residency_tests`.
pub(crate) async fn active_projects_core(state: &Arc<DaemonState>) -> RouteOutcome {
    let mgr = state.session_manager().await;
    let sessions: Vec<SessionRecord> = mgr.list().await;

    // Fail closed (#6836): a probe failure must never be read as "nothing is
    // active" — it degrades to trusting the PERSISTED state instead.
    let live_tmux_names: Option<HashSet<String>> = match mgr.tmux_driver().list_sessions() {
        Ok(names) => Some(names.into_iter().collect()),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "mpm.residency.active: tmux list_sessions failed; treating every \
                 persisted Active/Provisioning session as active (fail closed)"
            );
            None
        }
    };

    let mut groups: HashMap<PathBuf, ProjectAccum> = HashMap::new();
    for record in &sessions {
        let persisted_active = matches!(
            record.state,
            ManagedSessionState::Active | ManagedSessionState::Provisioning
        );
        if !persisted_active {
            continue;
        }
        let tmux_confirmed = live_tmux_names
            .as_ref()
            .is_none_or(|names| names.contains(&record.tmux_name));
        if !tmux_confirmed {
            continue;
        }

        let root = record
            .workspace_path
            .clone()
            .unwrap_or_else(|| record.cwd.clone());
        let last_activity_unix = record
            .last_activity_at
            .map(|dt| dt.timestamp().max(0) as u64);

        let accum = groups.entry(root.clone()).or_insert_with(|| ProjectAccum {
            root: root.clone(),
            repo_url: None,
            session_ids: Vec::new(),
            session_id_strings: Vec::new(),
            last_activity_unix: None,
        });
        if accum.repo_url.is_none() {
            accum.repo_url = record.repo_url.clone();
        }
        accum.session_ids.push(record.id);
        accum.session_id_strings.push(record.id.to_string());
        accum.last_activity_unix = match (accum.last_activity_unix, last_activity_unix) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) => Some(a),
            (None, b) => b,
        };
    }

    let mut projects = Vec::with_capacity(groups.len());
    for accum in groups.into_values() {
        let (palace_id, index_ids) = derive_for_group(&mgr, &accum).await;
        // `ActiveProject` is `#[non_exhaustive]`: struct-literal syntax (even
        // with `..Default::default()`) is refused outside its defining crate,
        // so build the default and assign fields.
        let mut project = ActiveProject::default();
        project.root = accum.root;
        project.palace_id = palace_id;
        project.index_ids = index_ids;
        project.session_ids = accum.session_id_strings;
        project.last_activity_unix = accum.last_activity_unix;
        projects.push(project);
    }

    let published_at_unix = chrono::Utc::now().timestamp().max(0) as u64;
    let mut set = ActiveProjectSet::default();
    set.schema = ACTIVE_PROJECT_SET_SCHEMA;
    set.generation = mgr.residency_generation();
    set.published_at_unix = published_at_unix;
    set.projects = projects;
    RouteOutcome::ok(&set)
}

/// Derive `(palace_id, index_ids)` for one project group, once — reusing the
/// manager's per-session cache when the group's representative session's
/// cached root still matches.
///
/// Why one representative rather than deriving per session in the group: two
/// sessions sharing a root always resolve to the same palace/index ids (both
/// are functions of `root` alone), so deriving once per GROUP is correct and
/// cheaper. The result is still cached under EVERY session id in the group,
/// so a later poll hits regardless of which session in the group survives.
async fn derive_for_group(
    mgr: &crate::session_manager::SessionManager,
    accum: &ProjectAccum,
) -> (Option<String>, Vec<String>) {
    let representative = accum.session_ids[0];
    if let Some(cached) = mgr
        .residency_cache_lookup(&representative, &accum.root)
        .await
    {
        return cached;
    }

    let palace_id =
        crate::core::session_launch::resolve_palace_slug(&accum.root, accum.repo_url.as_deref());

    let mut index_ids = vec![trusty_common::project_index_id::derive_project_index_id(
        &accum.root,
    )];
    if crate::core::worktree_index::is_git_worktree(&accum.root)
        && let Some(base) = crate::core::base_facet_index::base_checkout_of(&accum.root)
    {
        index_ids.push(trusty_common::project_index_id::derive_project_index_id(
            &base,
        ));
    }

    for &id in &accum.session_ids {
        mgr.residency_cache_store(id, accum.root.clone(), palace_id.clone(), index_ids.clone())
            .await;
    }

    (palace_id, index_ids)
}

#[cfg(test)]
#[path = "residency_tests.rs"]
mod residency_tests;
