//! The one palace-create flow behind the MCP `palace_create` tool and
//! [`super::MemoryService::create_palace`] (#11823).
//!
//! Why: the two surfaces each validated a create against the folder name and
//! fell back to the daemon's own cwd, then overwrote any existing palace. One
//! flow keeps their answers identical: the shared resolver's name for the
//! client's project, an alias to a folder-named palace that already holds the
//! project's memory, and "exists" — never an overwrite — for a palace or live
//! alias already under the id.
//! What: [`create_palace`] takes a [`PalaceCreateRequest`] and answers a
//! [`PalaceCreateResult`], whose JSON is the wire shape of both surfaces:
//! `{"palace_id": <id>, "status": "created" | "exists" | "aliased",
//! "alias_of": <target>}`, with `alias_of` present only when the id resolves
//! through an alias. Refusals are [`PalaceCreateError`].
//! Test: `tests/palace_create_11823.rs`.

use std::path::Path;

use serde::{Deserialize, Serialize};
use trusty_common::palace_alias::{PalaceAliasStore, try_alias_target_if_absent};
use trusty_common::palace_resolve::{PalaceCreatePlan, PalaceCreatePlanError};

use crate::engine::PalaceCreateOutcome;
use crate::engine::palace::{Palace, PalaceId};
use crate::project_root::{PERSONAL_PALACE, plan_palace_create_for_project};
use crate::{ActivitySource, AppState, DaemonEvent};

/// One create request, as either surface received it.
#[derive(Debug, Clone)]
pub struct PalaceCreateRequest<'a> {
    /// The palace id asked for.
    pub name: &'a str,
    /// Optional description stored in a NEW palace's metadata.
    pub description: Option<String>,
    /// The client's project directory. Required unless `force` or `personal`.
    pub cwd: Option<&'a str>,
    /// Skip the project-name check (authz-gated, spec-001).
    pub force: bool,
}

/// What a create did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PalaceCreateStatus {
    /// A new palace was written.
    Created,
    /// A palace, or a live alias, already held the id; nothing was written.
    Exists,
    /// The id was registered as an alias of the project's folder-named palace.
    Aliased,
}

/// The answer both surfaces return for a create that did not fail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PalaceCreateResult {
    /// The id the caller asked for.
    pub palace_id: String,
    /// What the create did.
    pub status: PalaceCreateStatus,
    /// The palace `palace_id` resolves to, when it is an alias.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias_of: Option<String>,
}

/// Why a create was refused. Nothing was written.
// #9273: a new refusal kind must not break a downstream exhaustive match.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PalaceCreateError {
    /// The request cannot name this palace: no `cwd`, an untrustworthy pin,
    /// no project directory, or a name that is not the project's palace.
    #[error("{0}")]
    Invalid(String),
    /// `force=true` was not authorized.
    #[error(transparent)]
    Forbidden(anyhow::Error),
    /// A lookup or a store write failed.
    #[error(transparent)]
    Store(anyhow::Error),
}

/// Create the palace a client asked for, or report the one already there.
///
/// Why: see the module docs (#11823).
/// What: `force` is authorized first. Unless `force`, the `personal` sentinel
/// or the test bypass (`TRUSTY_SKIP_PALACE_ENFORCEMENT=1`) applies, `cwd` is
/// required (no daemon-cwd or data-root fallback) and `name` must equal
/// `plan_palace_create_for_project`'s id for it; the plan aliases `name` to an
/// existing folder-named palace instead of creating one. Every other path goes
/// through `PalaceRegistry::create_palace_or_existing`, which never rewrites
/// an existing `palace.json`. A failed lookup refuses the create.
/// Test: `create_without_cwd_is_rejected`,
/// `create_on_an_existing_palace_answers_exists_and_leaves_it_untouched`,
/// `create_aliases_the_folder_named_palace`,
/// `create_aliases_through_a_folder_alias_to_its_target`,
/// `daemon_palace_env_does_not_change_a_client_create`,
/// `create_with_a_malformed_pin_is_rejected`.
pub fn create_palace(
    state: &AppState,
    req: PalaceCreateRequest<'_>,
    source: ActivitySource,
) -> Result<PalaceCreateResult, PalaceCreateError> {
    let name = req.name;
    if req.force {
        crate::authz::authorize_force_palace_create(state).map_err(PalaceCreateError::Forbidden)?;
    }
    let skip_enforcement = std::env::var("TRUSTY_SKIP_PALACE_ENFORCEMENT").as_deref() == Ok("1");
    if !skip_enforcement
        && !req.force
        && name != PERSONAL_PALACE
        && let Some(result) = plan_project_create(state, name, req.cwd)?
    {
        return Ok(result);
    }
    let palace = Palace {
        id: PalaceId::new(name),
        name: name.to_string(),
        description: req.description.filter(|s| !s.is_empty()),
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join(name),
    };
    let outcome = state
        .registry
        .create_palace_or_existing(&state.data_root, palace)
        .map_err(|e| PalaceCreateError::Store(e.context("create_palace")))?;
    let (status, alias_of) = match outcome {
        PalaceCreateOutcome::Created(_) => {
            // Issue #228: keep the in-memory palace-name cache in sync.
            state
                .palace_names
                .insert(name.to_string(), name.to_string());
            state.emit(DaemonEvent::PalaceCreated {
                id: name.to_string(),
                name: name.to_string(),
                source,
            });
            (PalaceCreateStatus::Created, None)
        }
        PalaceCreateOutcome::Exists(_) => (PalaceCreateStatus::Exists, None),
        // #11823: an existing alias is success; #9544 still forbids shadowing it.
        PalaceCreateOutcome::LiveAlias { target } => (PalaceCreateStatus::Exists, Some(target)),
    };
    Ok(PalaceCreateResult {
        palace_id: name.to_string(),
        status,
        alias_of,
    })
}

/// Validate `name` against the client's project and apply an alias plan.
///
/// What: `Ok(Some(aliased))` when the alias was registered, `Ok(None)` when
/// the caller should continue to the registry create, `Err` on any refusal.
fn plan_project_create(
    state: &AppState,
    name: &str,
    cwd: Option<&str>,
) -> Result<Option<PalaceCreateResult>, PalaceCreateError> {
    // #11823 (Q5): no project directory, no project palace.
    let cwd = cwd.filter(|s| !s.trim().is_empty()).ok_or_else(|| {
        PalaceCreateError::Invalid(format!(
            "palace_create '{name}': `cwd` is required — pass the project directory \
             the palace belongs to (or use 'personal')"
        ))
    })?;
    // #11823: trusty-common's env-free plan; a lookup error refuses (Fail-Open
    // check), it never reads as "absent".
    let plan = plan_palace_create_for_project(Path::new(cwd), |id| {
        palace_or_alias_exists(&state.data_root, id)
    })
    .map_err(|e| match e {
        PalaceCreatePlanError::Lookup { .. } => PalaceCreateError::Store(anyhow::Error::new(e)),
        other => PalaceCreateError::Invalid(format!("palace_create '{name}': {other}")),
    })?;
    let expected = match &plan {
        PalaceCreatePlan::Exists { id } | PalaceCreatePlan::Create { id } => id,
        PalaceCreatePlan::AliasToFolder { resolved, .. } => resolved,
        _ => {
            return Err(PalaceCreateError::Invalid(format!(
                "palace_create '{name}': unsupported create plan {plan:?}"
            )));
        }
    };
    if expected != name {
        return Err(PalaceCreateError::Invalid(format!(
            "palace name '{name}' does not match the project palace '{expected}' (resolved from {cwd})"
        )));
    }
    let PalaceCreatePlan::AliasToFolder { resolved, folder } = plan else {
        return Ok(None);
    };
    // #11823 (Q1): link, never rename, delete or shadow the folder palace.
    // Alias resolution is one hop, so when the folder id is itself a live
    // alias, link straight to its target or the new alias would be dead.
    let target = try_alias_target_if_absent(&state.data_root, &folder)
        .map_err(|e| PalaceCreateError::Store(e.context("resolve the folder palace alias")))?
        .unwrap_or(folder);
    PalaceAliasStore::register_alias(&state.data_root, &resolved, &target)
        .map_err(|e| PalaceCreateError::Store(e.context("register palace alias")))?;
    tracing::info!(alias = %resolved, target = %target, "palace_create: aliased to the folder-named palace");
    Ok(Some(PalaceCreateResult {
        palace_id: resolved,
        status: PalaceCreateStatus::Aliased,
        alias_of: Some(target),
    }))
}

/// Whether `id` names a palace (`palace.json` present) or a live alias.
fn palace_or_alias_exists(data_root: &Path, id: &str) -> anyhow::Result<bool> {
    if data_root.join(id).join("palace.json").try_exists()? {
        return Ok(true);
    }
    Ok(try_alias_target_if_absent(data_root, id)?.is_some())
}
