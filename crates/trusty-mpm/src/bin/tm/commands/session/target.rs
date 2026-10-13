//! Managed-session target resolution for `pause`, `events`, `run` and
//! `activity` (#9834), and the combined project + managed resolution for
//! `info` (#9862).
//!
//! Why: those verbs resolved only project sessions or sent the raw argument,
//! so a managed session name read "not found" or reached the wrong route.
//! `session.rs` sits near the 500-SLOC cap, so the logic lives here.
//! What: [`resolve`] classifies a target against the managed list and fails
//! closed on a list error; [`project_only`] refuses a managed target for a
//! verb with no managed route; [`activity`] reads a managed session's activity
//! by canonical id; [`not_found`] is the shared 404 wording; [`info`]
//! resolves over both session lists at once.
//! Test: `target_tests.rs`.

use trusty_mpm::client::{DaemonClient, Resolvable, TargetMatch, classify_target};

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

/// A `{ "sessions": [...] }` list body, kept as raw JSON rows.
#[derive(serde::Deserialize)]
struct ListBody {
    sessions: Vec<serde_json::Value>,
}

/// One session row `info` resolves over (#9862): its id, its name, and the
/// raw JSON printed on a match, so unknown fields survive.
struct Row {
    id: String,
    name: String,
    raw: serde_json::Value,
}

impl Resolvable for Row {
    fn id_matches(&self, query: &str) -> bool {
        self.id == query
    }
    fn resolve_name(&self) -> &str {
        &self.name
    }
}

/// Key each raw row by its string `id` and its `name_key` field.
fn rows(list: ListBody, name_key: &str) -> Vec<Row> {
    let text = |raw: &serde_json::Value, key: &str| {
        raw.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    list.sessions
        .into_iter()
        .map(|raw| Row {
            id: text(&raw, "id"),
            name: text(&raw, name_key),
            raw,
        })
        .collect()
}

/// The project session list; a failure is an error, as before #9862.
async fn project_rows(daemon: &DaemonClient) -> anyhow::Result<Vec<Row>> {
    let list: ListBody = daemon
        .get("/sessions")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(rows(list, "tmux_name"))
}

/// The managed session list; every failure is the list error (#9862).
async fn managed_rows(daemon: &DaemonClient, id_or_name: &str) -> anyhow::Result<Vec<Row>> {
    let fail = |cause: &dyn std::fmt::Display| {
        anyhow::anyhow!("could not list managed sessions to resolve '{id_or_name}': {cause}")
    };
    let resp = daemon
        .get("/api/v1/sessions/managed")
        .send()
        .await
        .map_err(|e| fail(&e))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(fail(
            &"the daemon has no managed-session route (HTTP 404); it is older than this CLI, so update and restart it",
        ));
    }
    let list: ListBody = resp
        .error_for_status()
        .map_err(|e| fail(&e))?
        .json()
        .await
        .map_err(|e| fail(&e))?;
    Ok(rows(list, "name"))
}

/// `tm sessions info <id|name|prefix>`: the raw JSON of the one matching
/// project or managed session.
///
/// Why (#9862): `info` matched only an exact id or name, read the project id
/// at a field the bare-string UUID never has, and turned every managed-list
/// failure into "not found".
/// What: fetches both lists, then runs ONE [`classify_target`] over the
/// project rows followed by the managed rows: id-exact, name-exact, unique
/// name-prefix, with a project row winning an exact tie. A prefix matching
/// two or more rows in either store is an error naming them. A project-list
/// failure is the HTTP error; a managed-list failure, 404 included, is
/// `could not list managed sessions to resolve '<x>': <cause>`.
/// Test: `info_*_9862` in `target_tests.rs`.
pub(crate) async fn info(
    daemon: &DaemonClient,
    id_or_name: &str,
) -> anyhow::Result<serde_json::Value> {
    let (project, managed) = tokio::join!(project_rows(daemon), managed_rows(daemon, id_or_name));
    let mut all = project?;
    all.extend(managed?);
    match classify_target(&all, id_or_name) {
        TargetMatch::Found(row) => Ok(row.raw.clone()),
        TargetMatch::Ambiguous(rows) => {
            let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
            Err(not_found(id_or_name, Some(names.join(", "))))
        }
        TargetMatch::NotFound => Err(not_found(id_or_name, None)),
    }
}

#[cfg(test)]
#[path = "target_tests.rs"]
mod target_tests;
