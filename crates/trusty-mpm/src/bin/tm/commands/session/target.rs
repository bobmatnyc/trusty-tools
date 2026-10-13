//! Managed-session target resolution for `pause`, `events`, `run` and
//! `activity` (#9834).
//!
//! Why: those verbs resolved only project sessions or sent the raw argument,
//! so a managed session name read "not found" or reached the wrong route.
//! `session.rs` sits near the 500-SLOC cap, so the logic lives here.
//! What: [`resolve`] classifies a target against the managed list and fails
//! closed on a list error; [`project_only`] refuses a managed target for a
//! verb with no managed route; [`activity`] reads a managed session's activity
//! by canonical id; [`not_found`] is the shared 404 wording.
//! Test: `target_tests.rs`.

use trusty_mpm::client::{DaemonClient, TargetMatch, classify_target};

/// How a target classified against the managed session list.
enum Resolved {
    /// Exactly one managed session matched; carries its canonical id.
    Managed(String),
    /// Two or more managed sessions share the prefix; carries their names.
    Ambiguous(String),
    /// No managed session matched.
    Unmanaged,
}

/// A project-session verb that has no managed-session route.
#[derive(Clone, Copy)]
pub(crate) enum ProjectVerb {
    Pause,
    Events,
    Run,
}

impl ProjectVerb {
    /// The verb name and the managed alternative named in the refusal.
    fn words(self, id: &str) -> (&'static str, String) {
        match self {
            Self::Pause => (
                "pause",
                format!(
                    "`tm sessions stop {id}` (managed runtime-stop; undo with `tm sessions resume {id}`)"
                ),
            ),
            Self::Events => (
                "events",
                format!("`tm sessions activity {id}` or `tm sessions output {id}`"),
            ),
            Self::Run => ("run", format!("`tm sessions send {id} <text>`")),
        }
    }
}

/// Classify `id_or_name` against the managed session list.
///
/// Why (#9834): a managed-list failure must never read as "not found" or fall
/// through to the project route, so this does not use `resolve_managed_match`,
/// which swallows the list error.
/// What: lists managed sessions (a failure is the error
/// `could not list managed sessions to resolve '<x>': <cause>`), then applies
/// the shared precedence: id-exact, name-exact, unique name-prefix.
/// Test: `*_list_error_never_reads_as_not_found_9834`.
async fn resolve(daemon: &DaemonClient, id_or_name: &str) -> anyhow::Result<Resolved> {
    let managed = daemon.list_managed_sessions().await.map_err(|e| {
        anyhow::anyhow!("could not list managed sessions to resolve '{id_or_name}': {e}")
    })?;
    Ok(match classify_target(&managed, id_or_name) {
        TargetMatch::Found(m) => Resolved::Managed(m.id.clone()),
        TargetMatch::Ambiguous(all) => {
            let names: Vec<&str> = all.iter().map(|m| m.name.as_str()).collect();
            Resolved::Ambiguous(names.join(", "))
        }
        TargetMatch::NotFound => Resolved::Unmanaged,
    })
}

/// Gate a project-only verb: refuse a managed target before any request.
///
/// Why (#9834): managed sessions have no pause, events or command route, so
/// sending the project request reads as "not found" or acts on a project
/// session of the same name. A managed match wins a name clash, as for Stop
/// and Resume.
/// What: `Err` naming the alternative for a managed match, or the [`resolve`]
/// list error; `Ok(Some(names))` for an ambiguous managed prefix (the caller
/// still tries the project route, where an exact project name wins);
/// `Ok(None)` when nothing managed matched.
/// Test: `*_on_managed_name_is_refused*_9834`,
/// `ambiguous_managed_prefix_with_project_404_is_ambiguity_9834`.
pub(crate) async fn project_only(
    daemon: &DaemonClient,
    id_or_name: &str,
    verb: ProjectVerb,
) -> anyhow::Result<Option<String>> {
    match resolve(daemon, id_or_name).await? {
        Resolved::Managed(id) => {
            let (name, alternative) = verb.words(&id);
            anyhow::bail!(
                "session '{id_or_name}' is a managed session; {name} is not supported for managed sessions; use {alternative}"
            )
        }
        Resolved::Ambiguous(names) => Ok(Some(names)),
        Resolved::Unmanaged => Ok(None),
    }
}

/// The error for a target the project route did not find: the managed
/// ambiguity when there was one, else "not found".
pub(crate) fn not_found(id_or_name: &str, ambiguous: Option<String>) -> anyhow::Error {
    match ambiguous {
        Some(names) => anyhow::anyhow!(
            "session '{id_or_name}' is ambiguous: it matches {names}; use a full name or id"
        ),
        None => anyhow::anyhow!("session '{id_or_name}' not found"),
    }
}

/// `tm sessions activity <id|name|prefix>`.
///
/// Why (#9834): the raw argument went straight into the activity path, so a
/// managed session name or prefix read "not found".
/// What: a managed match reads `/api/v1/sessions/managed/{id}/activity` by
/// canonical id; an ambiguous prefix is an error naming the candidates and
/// sends nothing; no match passes the raw argument through, so the daemon
/// answers its own 404.
/// Test: `activity_*_9834`.
pub(crate) async fn activity(daemon: &DaemonClient, id_or_name: String) -> anyhow::Result<()> {
    let id = match resolve(daemon, &id_or_name).await? {
        Resolved::Managed(id) => id,
        Resolved::Ambiguous(names) => return Err(not_found(&id_or_name, Some(names))),
        Resolved::Unmanaged => id_or_name,
    };
    crate::commands::managed::session_activity(daemon, id).await
}

#[cfg(test)]
#[path = "target_tests.rs"]
mod target_tests;
