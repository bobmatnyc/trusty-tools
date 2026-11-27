//! Shut a decommissioned project's search index and memory palace down, keep
//! their data (#10989).
//!
//! Why: ruling Q1 and D8e (epic #10987) — decommission keeps every index's
//! data, but a decommissioned project should not hold RAM, a file watcher and
//! open redb files. trusty-search's decommission park and trusty-memory's
//! `memory.palace_unload` release them and delete nothing.
//! What: [`IndexShutdown`] is the one door tm's decommission reaches either
//! daemon through. Like `DestructiveIndexDelete` it is a capability with a
//! private constructor, but it can only send three methods —
//! `search.project.resolve`, `search.project.park {decommission: true}` and
//! `memory.palace_unload` — none of which deletes anything. [`ShutdownTarget`]
//! names what to shut down; [`IndexShutdownReport`] says what happened, item by
//! item, and [`IndexShutdownReport::still_loaded`] lists every item a failure
//! may have left loaded.
//! Test: `index_shutdown_tests.rs`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use trusty_common::memory_rpc::{self, MemoryRpcError};
use trusty_common::search_rpc::{self, SearchRpcError};

/// Resolve a project to its one live index.
pub(crate) const METHOD_RESOLVE: &str = "search.project.resolve";
/// Park an index (sent with `decommission: true` only).
pub(crate) const METHOD_PARK: &str = "search.project.park";
/// Close one palace, keep its data.
pub(crate) const METHOD_PALACE_UNLOAD: &str = "memory.palace_unload";

/// Per-call budget. A park waits on a vector-store save; an unload waits up to
/// 10 s on a palace's write lock.
pub(super) const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Bound on the "is anything listening" connect.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// What one item's shutdown did (#10989).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ShutdownState {
    /// The daemon confirmed the item is unloaded (`id` is the index or palace).
    Unloaded {
        /// The index id or palace id.
        id: String,
    },
    /// Nothing was loaded: the daemon is not running, or holds no such item.
    NotLoaded {
        /// Why nothing was loaded.
        reason: String,
    },
    /// No call was made: this is a test process, or (#11643) the palace id
    /// is not provably this project's own.
    Skipped {
        /// Why no call was made.
        reason: String,
    },
    /// The shutdown failed; the item may still be loaded.
    Failed {
        /// The item, when it was identified.
        id: Option<String>,
        /// What failed.
        error: String,
    },
}

/// What [`IndexShutdown::shut_down`] did for one project (#10989).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexShutdownReport {
    /// The project's trusty-search index.
    pub search: ShutdownState,
    /// The project's trusty-memory palace.
    pub palace: ShutdownState,
}

impl IndexShutdownReport {
    /// Both items skipped for `reason`.
    pub fn skipped(reason: &str) -> Self {
        let state = ShutdownState::Skipped {
            reason: reason.to_string(),
        };
        Self {
            search: state.clone(),
            palace: state,
        }
    }

    /// Both items failed with `error` (nothing was attempted).
    pub fn failed(error: &str) -> Self {
        let state = ShutdownState::Failed {
            id: None,
            error: error.to_string(),
        };
        Self {
            search: state.clone(),
            palace: state,
        }
    }

    /// One line per failed item: what may still be loaded, and why.
    ///
    /// Test: `a_failed_park_is_listed_as_still_loaded`.
    pub fn still_loaded(&self) -> Vec<String> {
        [
            ("search index", &self.search),
            ("memory palace", &self.palace),
        ]
        .into_iter()
        .filter_map(|(what, state)| match state {
            ShutdownState::Failed { id, error } => Some(match id {
                Some(id) => format!("{what} {id}: {error}"),
                None => format!("{what}: {error}"),
            }),
            _ => None,
        })
        .collect()
    }
}

/// What to shut down for one project (#10989).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownTarget {
    /// The `search.project.resolve` queries, tried in order until one hits:
    /// the `owner/repo` identity, then the checkout paths.
    pub search_queries: Vec<String>,
    /// The palace id; `Ok(None)` when no checkout is on disk and neither the
    /// remote nor a recorded path derives an id (#11551); `Err` when an
    /// existing checkout fails to resolve.
    pub palace: Result<Option<String>, String>,
    /// #11551: the id was derived without a checkout, so ask trusty-memory
    /// whether the palace exists before unloading it.
    pub palace_lookup: bool,
    /// #11643: why the palace id may name another project's palace; when set,
    /// the unload is skipped and this is the reported reason.
    pub palace_shared: Option<String>,
    /// #11977: each other active project's name and its different derived
    /// palace id; the unload is skipped when trusty-memory's alias map sends
    /// one of them to this target's canonical palace.
    pub palace_peers: Vec<(String, String)>,
}

impl ShutdownTarget {
    /// The target for a project with registry key `project_key`, remote
    /// `repo_url`, and a session checkout `root` (if any is on disk).
    ///
    /// What: the queries are the `owner/repo` identity ([`repo_identity`]),
    /// then `root`, then `repo_url` when it is an absolute path, deduplicated.
    /// The palace resolves through `trusty_common::palace_resolve` from `root`
    /// and `repo_url`, the same call session launch exports
    /// `TRUSTY_MEMORY_PALACE` from. With no `root`, the id the same derivation
    /// gave from what is recorded: the remote's `owner/repo` slug (level 3),
    /// else the `parent-dir` slug of a path-form `repo_url` (level 4), else
    /// `Ok(None)`; `palace_lookup` is then set.
    /// Test: `target_prefers_the_project_key_then_the_remote_then_the_path`,
    /// `decommission_project_with_a_deleted_github_clone_unloads_its_palace`,
    /// `decommission_scratch_project_without_checkout_unloads_its_palace`,
    /// `a_path_project_is_parked_by_its_path`.
    pub fn for_project(project_key: Option<&str>, repo_url: &str, root: Option<&Path>) -> Self {
        let path_url = Some(repo_url.trim()).filter(|u| u.starts_with('/'));
        let mut search_queries: Vec<String> = Vec::new();
        let candidates = repo_identity(project_key, repo_url)
            .into_iter()
            .chain(root.map(|r| r.to_string_lossy().into_owned()))
            .chain(path_url.map(str::to_string));
        for query in candidates {
            if !search_queries.contains(&query) {
                search_queries.push(query);
            }
        }
        Self {
            search_queries,
            palace: palace_for(repo_url, root),
            palace_lookup: root.is_none(),
            palace_shared: None,
            palace_peers: Vec::new(),
        }
    }

    /// Refuse the palace unload unless no other project can own the same id.
    ///
    /// Why: #11643 — the `parent-dir` and `owner/repo` slugs key on the last
    /// two path parts, so `/a/code/app` and `/b/code/app` both derive
    /// `code-app`; unloading by that id would close the other project's palace.
    /// What: `others` is each other active project's name and [`palace_for`]
    /// answer. One deriving this target's id, or one whose id cannot be
    /// derived (`Err`, so it might match), sets `palace_shared`, which makes
    /// the unload a reported [`ShutdownState::Skipped`]. An `Ok(None)` names
    /// no palace and cannot collide. A target with no id is left as is.
    /// #11977: every other different `Ok(Some)` id is kept in `palace_peers`,
    /// which the unload compares by canonical id (see [`shared_canonical`]).
    /// Test: `a_palace_id_another_project_derives_is_never_unloaded`,
    /// `decommission_skips_the_unload_when_another_project_shares_the_palace_id`.
    pub fn refuse_shared_palace(
        mut self,
        others: &[(String, Result<Option<String>, String>)],
    ) -> Self {
        let Ok(Some(id)) = &self.palace else {
            return self;
        };
        // #11643: the first ambiguous project wins; a miss on all is the
        // only state in which the unload proceeds.
        self.palace_shared = others.iter().find_map(|(name, other)| match other {
            Ok(Some(other)) if other == id => Some(format!(
                "project '{name}' derives the same palace id {id}; palace unload skipped"
            )),
            Err(why) => Some(format!(
                "project '{name}' has no resolvable palace id ({why}), so {id} may be \
                 its palace; palace unload skipped"
            )),
            _ => None,
        });
        // #11977: derived ids differ, but an alias may join them; the unload
        // asks trusty-memory's alias map before it sends anything.
        let id = id.clone();
        self.palace_peers = others
            .iter()
            .filter_map(|(name, other)| match other {
                Ok(Some(other)) if *other != id => Some((name.clone(), other.clone())),
                _ => None,
            })
            .collect();
        self
    }
}

/// The palace id of a project with remote `repo_url` and checkout `root`.
///
/// What: with a `root`, `trusty_common::palace_resolve` from `root` and
/// `repo_url`, the call session launch exports `TRUSTY_MEMORY_PALACE` from;
/// `Err` when it fails. With none, [`palace_id_without_checkout`].
pub(crate) fn palace_for(repo_url: &str, root: Option<&Path>) -> Result<Option<String>, String> {
    let remote = Some(repo_url).filter(|u| !u.trim().is_empty());
    match root {
        Some(root) => trusty_common::palace_resolve::resolve_palace_with_remote(root, remote)
            .map(|r| Some(r.id))
            .map_err(|e| format!("could not resolve the palace: {e}")),
        // #11551: with no checkout, derive the id memory keyed the palace
        // by from what is recorded — the pin file went with the checkout.
        None => Ok(palace_id_without_checkout(repo_url)),
    }
}

/// The palace id of a project whose checkout is gone (#11551): the remote's
/// `owner/repo` slug, else the `parent-dir` slug of a path-form `repo_url`
/// (`~` expanded), the levels `palace_resolve` would have used.
fn palace_id_without_checkout(repo_url: &str) -> Option<String> {
    let url = repo_url.trim();
    if url.is_empty() {
        return None;
    }
    if !matches!(url.chars().next(), Some('/' | '~')) {
        return trusty_common::palace_id::owner_repo_from_git_remote(url);
    }
    crate::project::local_checkout_for_url(url)
        .and_then(|p| trusty_common::palace_id::parent_dir_slug(&p))
}

/// A project's `owner/repo` identity: `project_key` when it is one, else
/// [`crate::project::project_key_from_repo_url`] of `repo_url`; `None` for a
/// `local:` key with no GitHub remote.
///
/// #10989 F1: a path `repo_url` is never parsed as a URL — `/a/b/work/tools`
/// would invent `work/tools`; its identity is its checkout's origin, if any.
pub(crate) fn repo_identity(project_key: Option<&str>, repo_url: &str) -> Option<String> {
    project_key
        .filter(|k| !k.starts_with("local:"))
        .and_then(trusty_common::repo_identity::RepoIdentity::parse)
        .map(|r| r.canonical())
        .or_else(|| crate::project::project_key_from_repo_url(repo_url))
}

/// The capability to shut a project's indexes down without deleting data.
///
/// Why: see the module doc. The fields are private and [`Self::acquire`] is
/// the only production constructor, so no caller can point it at a daemon of
/// its own choosing, and it has no method that sends a delete.
pub struct IndexShutdown {
    search: Option<PathBuf>,
    memory: Option<PathBuf>,
}

impl IndexShutdown {
    /// Acquire the capability, or `Err(reason)` in a test process.
    ///
    /// Why: unloading the operator's live index from `cargo test` would take
    /// a real project offline (#4743 applies to unloads as much as deletes).
    /// What: `Err` under `running_under_test_harness`; otherwise the two
    /// daemons' socket paths (`None` for one that cannot be resolved).
    pub fn acquire() -> Result<Self, String> {
        if trusty_common::running_under_test_harness() {
            return Err(format!(
                "test process: no trusty-search or trusty-memory call made (set {} to override)",
                trusty_common::test_harness::ALLOW_PRODUCTION_ENV
            ));
        }
        Ok(Self {
            search: search_rpc::search_socket().ok(),
            memory: memory_rpc::resolve_memory_socket().ok(),
        })
    }

    /// The capability against explicit sockets, for tests.
    #[cfg(test)]
    pub(crate) fn at(search: Option<PathBuf>, memory: Option<PathBuf>) -> Self {
        Self { search, memory }
    }

    /// Shut `target` down: park its search index, unload its palace.
    ///
    /// What: each item is independent; one failing never skips the other.
    /// Test: `shutdown_parks_and_unloads_and_never_deletes`.
    pub async fn shut_down(&self, target: &ShutdownTarget) -> IndexShutdownReport {
        IndexShutdownReport {
            search: park_search(self.search.as_deref(), &target.search_queries).await,
            palace: unload_palace(self.memory.as_deref(), target).await,
        }
    }
}

/// Whether a daemon accepts connections on `socket`.
///
/// Why: a missing socket or a refused connect means no daemon, so nothing is
/// loaded; a daemon that accepts and then fails a call is a failure.
/// What: `Ok(Some)` when the connect succeeds; `Ok(None)` for no socket path,
/// `NotFound` or `ConnectionRefused`; `Err` for any other connect error and
/// for the timeout (#10989 F4: those cannot tell that nothing is loaded).
/// Test: `an_unreachable_socket_is_a_failure_not_a_daemon_down`.
pub(super) async fn listening(socket: Option<&Path>) -> Result<Option<&Path>, String> {
    let Some(socket) = socket else {
        return Ok(None);
    };
    let connect = tokio::net::UnixStream::connect(socket);
    match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
        Ok(Ok(_)) => Ok(Some(socket)),
        Ok(Err(e))
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(None)
        }
        Ok(Err(e)) => Err(format!("could not connect to {}: {e}", socket.display())),
        Err(_) => Err(format!(
            "connecting to {} timed out after {CONNECT_TIMEOUT:?}",
            socket.display()
        )),
    }
}

/// One resolve answer: the index, a miss, or a failure.
enum Resolved {
    Index(String),
    Miss,
    Failed(String),
}

/// Ask `search.project.resolve` for `query`'s live index; `identity` is the
/// project's `owner/repo`, when it has one.
async fn resolve_index(socket: &Path, query: &str, identity: Option<&str>) -> Resolved {
    let params = json!({ "project": query });
    match search_rpc::call_at(socket, METHOD_RESOLVE, params, CALL_TIMEOUT).await {
        Ok(body) => match body.get("index_id").and_then(Value::as_str) {
            // #10989: a path resolves to the deepest indexed root containing
            // it; an ancestor's index is not this project's.
            Some(_) if Path::new(query).is_absolute() && !owns_path(&body, query, identity) => {
                Resolved::Miss
            }
            Some(id) => Resolved::Index(id.to_string()),
            None => Resolved::Failed(format!("{METHOD_RESOLVE} answered no index_id for {query}")),
        },
        // A miss: no live index — an earlier decommission parked it, or the
        // project was never indexed.
        Err(e)
            if e.downcast_ref::<SearchRpcError>()
                .is_some_and(SearchRpcError::is_not_found) =>
        {
            Resolved::Miss
        }
        Err(e) => Resolved::Failed(format!("{e:#}")),
    }
}

/// Whether a resolve answer for the path `query` is the project's own index.
///
/// What: true when the answer's `root_path` is `query` or lies under it, or its
/// `repo_identity` is `identity`. Paths compare component-wise after
/// normalising. An answer with no readable `root_path` is never accepted.
/// Test: `a_path_resolve_to_an_ancestor_index_is_a_miss_and_parks_nothing`,
/// `a_path_project_is_parked_by_its_path`.
fn owns_path(body: &Value, query: &str, identity: Option<&str>) -> bool {
    let Some(root) = body.get("root_path").and_then(Value::as_str) else {
        return false;
    };
    let normalise =
        |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| Path::new(p).components().collect());
    let same_identity =
        identity.is_some() && body.get("repo_identity").and_then(Value::as_str) == identity;
    normalise(root).starts_with(normalise(query)) || same_identity
}

/// Resolve the project's index, then park it for the decommission.
///
/// What: tries each query in order; a miss moves to the next (#10989 F1: an
/// identity miss retries by path), and only a miss on every query is
/// [`ShutdownState::NotLoaded`].
async fn park_search(socket: Option<&Path>, queries: &[String]) -> ShutdownState {
    let socket = match listening(socket).await {
        Ok(Some(socket)) => socket,
        Ok(None) => {
            return ShutdownState::NotLoaded {
                reason: "trusty-search is not running".to_string(),
            };
        }
        Err(error) => return ShutdownState::Failed { id: None, error },
    };
    // #11551: nothing to resolve means no index can be loaded for it.
    if queries.is_empty() {
        return ShutdownState::NotLoaded {
            reason: "the project has no repo identity or checkout path to resolve".to_string(),
        };
    }
    // The identity, when present, is the first query and is never a path.
    let identity = queries
        .first()
        .map(String::as_str)
        .filter(|q| !Path::new(q).is_absolute());
    let mut found = None;
    for query in queries {
        match resolve_index(socket, query, identity).await {
            Resolved::Index(id) => {
                found = Some(id);
                break;
            }
            Resolved::Miss => {}
            Resolved::Failed(error) => return ShutdownState::Failed { id: None, error },
        }
    }
    let Some(index_id) = found else {
        return ShutdownState::NotLoaded {
            reason: format!("no live index for {}", queries.join(" or ")),
        };
    };
    let params = json!({ "index_id": index_id, "decommission": true });
    match search_rpc::call_at(socket, METHOD_PARK, params, CALL_TIMEOUT).await {
        Ok(body) if is_true(&body, "parked") && is_true(&body, "unloaded") => {
            ShutdownState::Unloaded { id: index_id }
        }
        // #10989: a park the daemon did not confirm as unloaded is a failure.
        Ok(body) => ShutdownState::Failed {
            id: Some(index_id),
            error: format!("the park was not confirmed unloaded: {body}"),
        },
        Err(e) => ShutdownState::Failed {
            id: Some(index_id),
            error: format!("{e:#}"),
        },
    }
}

/// `memory.palace_get` — trusty-memory's "does this palace exist" lookup.
pub(crate) const METHOD_PALACE_GET: &str = "memory.palace_get";

/// #11551: ask trusty-memory whether `palace` exists before unloading it.
///
/// What: `None` when it exists (go on to the unload); `NotLoaded` naming the
/// skip on `CODE_NOT_FOUND`; `Failed` on any other answer — a lookup that
/// errors never reads as "absent" (fail closed).
/// Test: `decommission_scratch_project_with_no_palace_skips_the_unload`,
/// `decommission_scratch_project_palace_lookup_error_refuses`.
async fn palace_absent(socket: &Path, palace: &str) -> Option<ShutdownState> {
    let params = json!({ "palace_id": palace });
    let answer = memory_rpc::call_memory_tool_at_with_timeout(
        socket,
        METHOD_PALACE_GET,
        params,
        CALL_TIMEOUT,
    )
    .await;
    match answer {
        Ok(_) => None,
        Err(e) => Some(match e.downcast_ref::<MemoryRpcError>() {
            Some(r) if r.code == memory_rpc::CODE_NOT_FOUND => ShutdownState::NotLoaded {
                reason: format!("no palace {palace} in trusty-memory; palace unload skipped"),
            },
            _ => ShutdownState::Failed {
                id: Some(palace.to_string()),
                error: format!("could not look up palace {palace}: {e:#}"),
            },
        }),
    }
}

/// `memory.config` — answers the daemon's palace registry dir as `data_root`.
pub(crate) const METHOD_CONFIG: &str = "memory.config";

/// Why the palace unload must be skipped because `palace` and a peer's id
/// reach one canonical palace, or `None` when no peer shares it.
///
/// Why: #11977 — trusty-memory unloads by canonical id and follows the alias
/// map, so `code-app` and `work-app` can both reach a folder palace `app`.
/// What: asks `memory.config` for the daemon's `data_root` (the directory its
/// own unload resolves aliases in), then applies the shared rule
/// [`trusty_common::palace_alias::try_alias_target_if_absent`] to `palace`
/// and each peer id. A failed call, a missing `data_root` or an unreadable
/// alias file is a skip too (fail closed: a kept palace is safe, a shared
/// one unloaded is not).
/// Test: `an_aliased_palace_another_project_reaches_is_never_unloaded`,
/// `an_unresolvable_canonical_id_skips_the_unload`.
async fn shared_canonical(
    socket: &Path,
    palace: &str,
    peers: &[(String, String)],
) -> Option<String> {
    let answer = memory_rpc::call_memory_tool_at_with_timeout(
        socket,
        METHOD_CONFIG,
        json!({}),
        CALL_TIMEOUT,
    )
    .await;
    let root = match answer {
        Ok(body) => match body.get("data_root").and_then(Value::as_str) {
            Some(root) => PathBuf::from(root),
            None => {
                return Some(format!(
                    "{METHOD_CONFIG} named no data_root, so palace {palace}'s canonical id is \
                     unknown; palace unload skipped"
                ));
            }
        },
        Err(e) => {
            return Some(format!(
                "could not resolve palace {palace}'s canonical id ({e:#}); palace unload skipped"
            ));
        }
    };
    let canonical = |id: &str| {
        trusty_common::palace_alias::try_alias_target_if_absent(&root, id)
            .map(|target| target.unwrap_or_else(|| id.to_string()))
            .map_err(|e| format!("could not read the palace alias map for {id}: {e:#}"))
    };
    let mine = match canonical(palace) {
        Ok(mine) => mine,
        Err(why) => return Some(format!("{why}; palace unload skipped")),
    };
    peers.iter().find_map(|(name, id)| match canonical(id) {
        Ok(theirs) if theirs == mine => Some(format!(
            "project '{name}' palace {id} and {palace} both resolve to palace {mine}; \
             palace unload skipped"
        )),
        Ok(_) => None,
        Err(why) => Some(format!("{why} (project '{name}'); palace unload skipped")),
    })
}

/// Unload the project's palace.
async fn unload_palace(socket: Option<&Path>, target: &ShutdownTarget) -> ShutdownState {
    let palace = &target.palace;
    let socket = match listening(socket).await {
        Ok(Some(socket)) => socket,
        Ok(None) => {
            return ShutdownState::NotLoaded {
                reason: "trusty-memory is not running".to_string(),
            };
        }
        Err(error) => return ShutdownState::Failed { id: None, error },
    };
    let palace = match palace {
        Ok(Some(p)) => p,
        // #11551: no checkout and nothing recorded to derive a palace id from.
        Ok(None) => {
            return ShutdownState::NotLoaded {
                reason: "no checkout or recorded path names a palace; unload skipped".to_string(),
            };
        }
        Err(why) => {
            return ShutdownState::Failed {
                id: None,
                error: why.clone(),
            };
        }
    };
    // #11643: an id another project may own is never unloaded.
    if let Some(reason) = &target.palace_shared {
        return ShutdownState::Skipped {
            reason: reason.clone(),
        };
    }
    // #11977: a palace another project reaches through an alias is shared too.
    if !target.palace_peers.is_empty()
        && let Some(reason) = shared_canonical(socket, palace, &target.palace_peers).await
    {
        tracing::warn!(palace = %palace, "project decommission: {reason}");
        return ShutdownState::Skipped { reason };
    }
    if target.palace_lookup
        && let Some(skipped) = palace_absent(socket, palace).await
    {
        return skipped;
    }
    let params = json!({ "palace_id": palace });
    let answer = memory_rpc::call_memory_tool_at_with_timeout(
        socket,
        METHOD_PALACE_UNLOAD,
        params,
        CALL_TIMEOUT,
    )
    .await;
    match answer {
        Ok(body)
            if is_true(&body, "unloaded") && body.get("exists") == Some(&Value::Bool(false)) =>
        {
            ShutdownState::NotLoaded {
                reason: format!("palace {palace} is not on disk"),
            }
        }
        Ok(body) if is_true(&body, "unloaded") => ShutdownState::Unloaded { id: palace.clone() },
        Ok(body) => ShutdownState::Failed {
            id: Some(palace.clone()),
            error: format!("the unload was not confirmed: {body}"),
        },
        Err(e) => {
            let predates = e
                .downcast_ref::<MemoryRpcError>()
                .is_some_and(|r| r.code == trusty_common::uds::server::CODE_METHOD_NOT_FOUND);
            let error = if predates {
                format!(
                    "the trusty-memory daemon predates {METHOD_PALACE_UNLOAD}; restart it on a \
                     release that has it, then decommission again"
                )
            } else {
                format!("{e:#}")
            };
            ShutdownState::Failed {
                id: Some(palace.clone()),
                error,
            }
        }
    }
}

/// `body[key] == true`; an absent or non-boolean field is not affirmative.
pub(super) fn is_true(body: &Value, key: &str) -> bool {
    body.get(key).and_then(Value::as_bool) == Some(true)
}

// The rig is shared with `project_decommission_tests`.
#[cfg(test)]
#[path = "index_shutdown_tests.rs"]
pub(crate) mod tests;
