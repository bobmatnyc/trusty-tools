//! What [`super::ensure_project_indexed_reporting`] does when the derived index
//! id cannot name the tree it was asked to register (#6864).
//!
//! Why: [`crate::derive_index_id`] is the directory basename, and
//! trusty-search's registry is one-`root_path`-per-id. Two checkouts of one
//! repository therefore derive one id, and the second one to launch collides:
//! the daemon refuses with its conflict code (`root_path_mismatch_response`)
//! because the id already identifies the FIRST checkout. The client read that as
//! a plain failure, so the session got `NotConfirmed`, no pin, and every MCP
//! `search` call in it answered `missing required string field: index_id` —
//! while an index for the requested tree was sitting in the same daemon under
//! another id. On 2026-09-05 that other id was `trusty-tools-checkout` and the
//! session ran its whole length unable to search the tree it was working in.
//!
//! What: [`create_and_reconcile`] runs the find-or-create and, on a conflict,
//! resolves the id that ALREADY serves this root before giving up — first from
//! the `existing_id` a root-collision names, then from `search.indexes.list`
//! with `details: true`, matched on `root_path` via
//! [`crate::identifies_same_path`], and finally by registering the tree under
//! [`crate::derive_checkout_index_id`], the collision-resistant form #6149
//! already defined. The caller pins whatever id comes back, so a colliding
//! basename now costs one extra call rather than the whole session's search.
//!
//! #7237: every request here goes over the daemon's Unix socket
//! ([`crate::search_rpc`]) rather than `http://127.0.0.1:7878`. The one thing
//! that transport changes is where `existing_id` comes from: the socket error
//! frame carries a code and a message, not the refusal's JSON body, so tier one
//! reads the id out of the daemon's own message (see
//! [`existing_id_from_conflict`]) and degrades to the registry scan when it
//! cannot. Tier two is what actually guarantees the recovery.
//!
//! This is the trusty-common half of the same rule #6677 gave the trusty-review
//! report pass (`report::index_registry::resolve_report_index`): address the
//! index registered at this checkout's `root_path` when the derived id is not
//! it. That copy matches through the review crate's own `IndexInfo` and
//! `config::index_resolver::best_matching_index`, so routing it through here
//! would be a behavioural change to two crates rather than a move; it is left
//! alone deliberately.
//!
//! Every step stays best-effort: an unreachable or refusing daemon leaves the
//! registration `NotConfirmed` exactly as it did before, and the extra list call
//! carries the same ~1s cap as the create it follows.
//!
//! Test: the `tests` module below, plus
//! `registration_matches_an_existing_index_by_root_path` and
//! `registration_falls_back_to_a_collision_resistant_id` in
//! `search_index_tests.rs`.

use std::path::Path;
use std::time::Duration;

use super::{
    CREATE_TIMEOUT, IndexOptions, IndexRegistration, best_effort_create_index,
    registered_root_from_response,
};
use crate::search_rpc::{self, SearchRpcError};

/// Overall budget for the one registry read this recovery costs (#6864).
const LIST_TIMEOUT: Duration = CREATE_TIMEOUT;

/// The literal trusty-search puts before the id that already owns a `root_path`.
///
/// Why a substring of the daemon's own message rather than a body field: the
/// HTTP refusal carried `existing_id` beside the prose, and the socket's
/// `RpcError` carries only `{code, message}` — `rpc_error_from_http` drops
/// everything else — so the id survives the transport only in the wording
/// `root_path_collision_response` builds. This is a best-effort shortcut, not
/// the recovery: when the wording drifts, [`existing_id_from_conflict`] answers
/// `None` and [`resolve_colliding_id`] falls through to the registry scan, which
/// is what actually resolves the collision.
const COLLISION_MARKER: &str = "is already registered to index '";

/// What one create attempt achieved, with the conflict told apart (#6864).
///
/// Why: [`IndexRegistration`] collapses "the daemon refused" and "the daemon
/// holds this id, or this tree, under different terms" into `NotConfirmed`, and
/// only the second is recoverable. Separating them here is what lets
/// [`create_and_reconcile`] retry instead of stranding the session unpinned.
/// What: `Confirmed` is a successful reply naming this same tree; `Conflict` is
/// the daemon's conflict code or a `{created: false}` whose `root_path` is
/// another tree, carrying the `existing_id` when the daemon named one;
/// `NotConfirmed` is every other refusal, a transport error, a malformed reply,
/// and a panicked worker.
/// Test: `a_root_collision_message_carries_the_existing_id`,
/// plus `create_index_response_for_a_different_tree_reports_a_conflict` and
/// `create_index_response_for_the_same_tree_is_confirmed` in
/// `search_index_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CreateOutcome {
    /// The daemon acknowledged the index for the requested tree.
    Confirmed,
    /// The requested id cannot name this tree. `existing_id` is set when the
    /// daemon's refusal named the index that already owns this `root_path`.
    Conflict { existing_id: Option<String> },
    /// Nothing was registered and nothing here can recover it.
    NotConfirmed,
}

/// Read one successful `search.index.create` reply as a [`CreateOutcome`].
///
/// Why: "the daemon answered" never decided this. A `{created: false}` naming
/// another tree is a conflict wearing a success frame (#5065 review) — one index
/// identifies one directory tree, so a request naming a tree the daemon does not
/// hold under that id has not been satisfied. The rule that says so lives here,
/// beside the recovery, so the two cannot drift apart.
/// What: a reply whose reported `root_path` is another tree yields `Conflict`
/// with no `existing_id` (no index for this tree has been identified yet); every
/// other reply — including one this function cannot read at all — is
/// `Confirmed`, exactly as a body-less 2xx was. `root_display` is the requested
/// root as the caller already rendered it, for the log lines.
/// Test: `search_index_tests.rs::{create_index_response_for_a_different_tree_reports_a_conflict,
/// create_index_response_for_the_same_tree_is_confirmed,
/// a_malformed_create_reply_is_not_a_registration}`.
pub(super) fn classify_create_result(
    result: &serde_json::Value,
    index_id: &str,
    root: &Path,
    root_display: &str,
) -> CreateOutcome {
    match registered_root_from_response(result) {
        Some(registered) if !crate::identifies_same_path(Path::new(&registered), root) => {
            tracing::warn!(
                "trusty-search index '{index_id}' is registered at {registered}, not at the \
                 requested {root_display}; withholding confirmation so the caller cannot pin \
                 an index that searches a different tree"
            );
            CreateOutcome::Conflict { existing_id: None }
        }
        _ => {
            tracing::debug!("registered trusty-search index '{index_id}' (root={root_display})");
            CreateOutcome::Confirmed
        }
    }
}

/// Read one FAILED `search.index.create` call as a [`CreateOutcome`] (#7237).
///
/// Why: the daemon's conflict code is the recoverable refusal — it refuses to
/// re-register an id over a second tree, and refuses a second id over one tree —
/// and every other failure is not. Over HTTP that split was a status code; over
/// the socket it is [`SearchRpcError::is_conflict`], and a transport failure
/// carries no `SearchRpcError` at all.
/// What: the conflict code yields `Conflict`, carrying whatever id
/// [`existing_id_from_conflict`] can read out of the daemon's message; every
/// other daemon refusal, and every transport or decode failure, yields
/// `NotConfirmed`. Both are logged at warn and swallowed — this function never
/// makes the call fallible.
/// Test: `search_index_tests.rs::{a_daemon_refusal_is_not_a_registration,
/// registration_matches_an_existing_index_by_root_path,
/// create_rejected_by_the_daemon_withholds_the_pinnable_id}`.
pub(super) fn classify_create_failure(
    err: &anyhow::Error,
    index_id: &str,
    root_display: &str,
) -> CreateOutcome {
    let Some(refusal) = err.downcast_ref::<SearchRpcError>() else {
        tracing::warn!("trusty-search index registration for '{index_id}' failed: {err:#}");
        return CreateOutcome::NotConfirmed;
    };
    if refusal.is_conflict() {
        tracing::warn!(
            "trusty-search index registration for '{index_id}' at {root_display} \
             conflicts ({}); looking for the index already serving that tree",
            refusal.message
        );
        return CreateOutcome::Conflict {
            existing_id: existing_id_from_conflict(&refusal.message),
        };
    }
    tracing::warn!("trusty-search index registration for '{index_id}' was refused: {refusal}");
    CreateOutcome::NotConfirmed
}

/// Find-or-create `index_id` for `root`, resolving a basename collision to the
/// index that already serves that tree (#6864).
///
/// Why: see the module doc. The returned id is what the caller PINS, so it must
/// name an index the daemon actually holds for this root — not the id that was
/// asked for.
/// What: runs [`best_effort_create_index`] against `socket`; a `Confirmed` or
/// unrecoverable answer returns the requested id unchanged, and a conflict goes
/// through [`resolve_colliding_id`]. Returns the id to pin alongside the
/// registration verdict; `IndexRegistration::Confirmed` is returned only when
/// some index for this root was acknowledged.
/// Test: `search_index_tests.rs::{registration_matches_an_existing_index_by_root_path,
/// registration_falls_back_to_a_collision_resistant_id,
/// create_rejected_by_the_daemon_withholds_the_pinnable_id}`.
pub(super) fn create_and_reconcile(
    socket: &Path,
    index_id: &str,
    root: &Path,
    opts: IndexOptions,
) -> (String, IndexRegistration) {
    match best_effort_create_index(socket, index_id, root, opts) {
        CreateOutcome::Confirmed => (index_id.to_string(), IndexRegistration::Confirmed),
        CreateOutcome::NotConfirmed => (index_id.to_string(), IndexRegistration::NotConfirmed),
        // #6864: the id is taken, or this tree is; ask which index serves it.
        CreateOutcome::Conflict { existing_id } => {
            match resolve_colliding_id(socket, index_id, root, opts, existing_id) {
                Some(resolved) => (resolved, IndexRegistration::Confirmed),
                None => (index_id.to_string(), IndexRegistration::NotConfirmed),
            }
        }
    }
}

/// The id of an index that serves `root`, after the derived id failed (#6864).
///
/// Why: three sources answer the same question and they are tried cheapest
/// first. The root-collision refusal already names the index owning this
/// `root_path` when the collision was on the tree, so no request is needed at
/// all. A collision on the ID instead needs the registry read, which is the one
/// extra round trip this fix costs. Only when neither finds an index for this
/// tree is a second create justified — and it uses
/// [`crate::derive_checkout_index_id`], the path-digest form #6149 defined for
/// exactly this, rather than a new scheme.
/// What: returns the id to pin, or `None` when nothing serves this root and the
/// fallback create did not land. Registering under the digest id can itself
/// conflict — a COLD registry entry for this tree is not listed by
/// `search.indexes.list` but is still checked by the daemon's root-collision
/// guard — so that answer's `existing_id` is honoured too.
/// Test: `a_root_collision_message_carries_the_existing_id` covers the message
/// read; the three-tier resolution is the two live-daemon tests named on
/// [`create_and_reconcile`].
fn resolve_colliding_id(
    socket: &Path,
    derived: &str,
    root: &Path,
    opts: IndexOptions,
    existing_id: Option<String>,
) -> Option<String> {
    if let Some(id) = existing_id {
        tracing::info!(
            "trusty-search already serves {} as index '{id}'; pinning that instead of \
             the derived '{derived}' (#6864)",
            root.display()
        );
        return Some(id);
    }

    if let Some(body) = fetch_index_list(socket)
        && let Some(id) = index_id_serving_root(&body, root)
    {
        tracing::info!(
            "trusty-search index '{derived}' identifies another tree; {} is registered \
             as '{id}' and that is what this session pins (#6864)",
            root.display()
        );
        return Some(id);
    }

    let fresh = crate::derive_checkout_index_id(root)?;
    tracing::info!(
        "no trusty-search index is registered for {}; registering it under the \
         collision-resistant id '{fresh}' because '{derived}' names another tree (#6864)",
        root.display()
    );
    match best_effort_create_index(socket, &fresh, root, opts) {
        CreateOutcome::Confirmed => Some(fresh),
        CreateOutcome::Conflict { existing_id } => existing_id,
        CreateOutcome::NotConfirmed => None,
    }
}

/// The id a root-collision refusal names as the current owner of a `root_path`.
///
/// Why: trusty-search answers a create whose `root_path` is already registered
/// with `root_path_collision_response`, which names the owning index precisely
/// so the caller does not have to cross-reference the registry by hand. Reading
/// it turns the most common recoverable conflict into zero extra requests. #7237
/// moved the read from that refusal's JSON body to its message, because the
/// socket's error frame carries only `{code, message}` — see
/// [`COLLISION_MARKER`] for why that is a shortcut rather than the guarantee.
/// What: the id between [`COLLISION_MARKER`] and the next `'`, or `None` for any
/// other message — including `root_path_mismatch_response`, the
/// same-id-different-tree refusal, which deliberately names no such index
/// because no index serves the requested tree.
/// Test: `a_root_collision_message_carries_the_existing_id`,
/// `a_root_mismatch_message_names_no_existing_index`.
pub(super) fn existing_id_from_conflict(message: &str) -> Option<String> {
    let (_, rest) = message.split_once(COLLISION_MARKER)?;
    let (id, _) = rest.split_once('\'')?;
    (!id.is_empty()).then(|| id.to_string())
}

/// The id of the registry entry whose `root_path` IS `root`.
///
/// Why: matching on the id is what created #6864; matching on the tree is what
/// resolves it. [`crate::identifies_same_path`] is the shared `(dev, ino)`
/// comparison the daemon's own collision guard uses, so a case-variant or
/// symlinked spelling of one tree matches here exactly as it does there.
/// What: scans `search.indexes.list`'s `indexes` array and returns the first
/// entry whose `root_path` names the same tree. Entries missing `id` or
/// `root_path` are skipped rather than failing the scan — the daemon omits
/// `root_path` for a non-UTF-8 root, and an entry that cannot be compared is
/// simply not a match.
/// Test: `index_id_serving_root_matches_on_the_tree_not_the_id`,
/// `index_id_serving_root_is_none_when_no_entry_matches`,
/// `index_id_serving_root_tolerates_a_malformed_body`.
pub(super) fn index_id_serving_root(body: &serde_json::Value, root: &Path) -> Option<String> {
    for entry in body.get("indexes")?.as_array()? {
        let Some(registered) = entry.get("root_path").and_then(|v| v.as_str()) else {
            continue;
        };
        if !crate::identifies_same_path(Path::new(registered), root) {
            continue;
        }
        if let Some(id) = entry.get("id").and_then(|v| v.as_str()) {
            return Some(id.to_string());
        }
    }
    None
}

/// Read the daemon's detailed index list, best-effort.
///
/// Why: `root_path` rides only on the detailed listing; the flat list is bare
/// ids and cannot answer which index serves a tree. This runs on the same hot
/// path as the create it follows, so it carries the create's ~1s cap and, via
/// [`crate::search_rpc::call_blocking`], its own dedicated OS thread — both
/// callers of `ensure_project_indexed*` are frequently async.
/// What: `search.indexes.list` with `{"details": true}`, returning the daemon's
/// `result` on success and `None` for a refusal or a transport failure. A `None`
/// here falls through to the fallback create rather than failing the
/// registration.
/// Test: covered through `registration_matches_an_existing_index_by_root_path`
/// in `search_index_tests.rs`, which serves this request from a fake daemon.
fn fetch_index_list(socket: &Path) -> Option<serde_json::Value> {
    match search_rpc::call_blocking(
        socket,
        search_rpc::METHOD_INDEXES_LIST,
        serde_json::json!({ "details": true }),
        LIST_TIMEOUT,
    ) {
        Ok(body) => Some(body),
        Err(e) => {
            tracing::warn!("trusty-search index list failed: {e:#}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The message `root_path_collision_response` builds, in the wording the
    /// daemon actually sends.
    fn collision_message(root: &str, existing_id: &str) -> String {
        format!(
            "root_path {root:?} is already registered to index '{existing_id}'; two \
             indexes cannot share one on-disk corpus (issues #2305, #2336)"
        )
    }

    /// Why: this is the zero-request recovery — the daemon already told us which
    /// index owns the tree, so reading it must not require a registry scan.
    /// Test: itself.
    #[test]
    fn a_root_collision_message_carries_the_existing_id() {
        assert_eq!(
            existing_id_from_conflict(&collision_message(
                "/Users/masa/checkout/trusty-tools",
                "trusty-tools-checkout"
            )),
            Some("trusty-tools-checkout".to_string())
        );
    }

    /// Why: the same-id-different-tree refusal names the OTHER checkout's root,
    /// not an index serving ours. Reading an id out of it would pin the very
    /// index #6864 is about not pinning. A message this parser does not
    /// recognise must degrade to `None`, which sends the caller to the registry
    /// scan rather than to a wrong id.
    /// Test: itself.
    #[test]
    fn a_root_mismatch_message_names_no_existing_index() {
        let mismatch = "index 'trusty-tools' is registered at \
             \"/Users/masa/Projects/trusty-tools\"; it cannot be re-registered at \
             \"/Users/masa/checkout/trusty-tools\" because one index identifies one \
             directory tree";
        assert_eq!(existing_id_from_conflict(mismatch), None);
        assert_eq!(existing_id_from_conflict(""), None);
        assert_eq!(
            existing_id_from_conflict("root_path is already registered to index '"),
            None,
            "an unterminated quote names no id"
        );
        assert_eq!(
            existing_id_from_conflict("root_path is already registered to index ''"),
            None,
            "an empty id is not an id"
        );
    }

    /// Why: the whole fix in one assertion — the entry that matches is the one
    /// whose ROOT is this tree, even though a different entry carries the id the
    /// basename derives.
    /// Test: itself.
    #[test]
    fn index_id_serving_root_matches_on_the_tree_not_the_id() {
        let mine = std::env::temp_dir();
        let body = serde_json::json!({
            "indexes": [
                { "id": "trusty-tools", "root_path": "/nonexistent/other/trusty-tools" },
                { "id": "trusty-tools-checkout", "root_path": mine.to_string_lossy() },
            ]
        });
        assert_eq!(
            index_id_serving_root(&body, &mine),
            Some("trusty-tools-checkout".to_string()),
            "the entry rooted at this tree is the one to pin, whatever its id"
        );
    }

    /// Why: no match must stay `None` so the caller registers rather than pinning
    /// somebody else's tree.
    /// Test: itself.
    #[test]
    fn index_id_serving_root_is_none_when_no_entry_matches() {
        let body = serde_json::json!({
            "indexes": [{ "id": "api", "root_path": "/nonexistent/work/api" }]
        });
        assert_eq!(
            index_id_serving_root(&body, Path::new("/nonexistent/work/other")),
            None
        );
    }

    /// Why: this reads a daemon reply on a hot path, so every malformed shape
    /// must degrade to "no match" rather than panic. An entry with a null
    /// `root_path` (a non-UTF-8 root) is skipped, not treated as a match.
    /// Test: itself.
    #[test]
    fn index_id_serving_root_tolerates_a_malformed_body() {
        let root = std::env::temp_dir();
        assert_eq!(
            index_id_serving_root(&serde_json::Value::String("nope".into()), &root),
            None
        );
        assert_eq!(index_id_serving_root(&serde_json::json!({}), &root), None);
        assert_eq!(
            index_id_serving_root(&serde_json::json!({ "indexes": "nope" }), &root),
            None
        );
        assert_eq!(
            index_id_serving_root(
                &serde_json::json!({ "indexes": [{ "id": "x", "root_path": null }] }),
                &root
            ),
            None
        );
        assert_eq!(
            index_id_serving_root(
                &serde_json::json!({ "indexes": [{ "root_path": root.to_string_lossy() }] }),
                &root
            ),
            None,
            "an entry with no id cannot be pinned"
        );
    }
}
