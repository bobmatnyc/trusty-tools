//! What [`super::ensure_project_indexed_reporting`] does when the derived index
//! id cannot name the tree it was asked to register (#6864).
//!
//! Why: [`crate::derive_index_id`] is the directory basename, and
//! trusty-search's registry is one-`root_path`-per-id. Two checkouts of one
//! repository therefore derive one id, and the second one to launch collides:
//! the daemon answers `409` (`root_path_mismatch_response`) because the id
//! already identifies the FIRST checkout. The client read that as a plain
//! failure, so the session got `NotConfirmed`, no pin, and every MCP `search`
//! call in it answered `missing required string field: index_id` — while an
//! index for the requested tree was sitting in the same daemon under another id.
//! On 2026-09-05 that other id was `trusty-tools-checkout` and the session ran
//! its whole length unable to search the tree it was working in.
//!
//! What: [`create_and_reconcile`] runs the find-or-create and, on a conflict,
//! resolves the id that ALREADY serves this root before giving up — first from
//! the `existing_id` a root-collision `409` names, then from
//! `GET /indexes?details=true` matched on `root_path` via
//! [`crate::identifies_same_path`], and finally by registering the tree under
//! [`crate::derive_checkout_index_id`], the collision-resistant form #6149
//! already defined. The caller pins whatever id comes back, so a colliding
//! basename now costs one extra GET rather than the whole session's search.
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
//! registration `NotConfirmed` exactly as it did before, and the extra GET
//! carries the same ~1s / 750 ms caps as the create it follows.
//!
//! Test: the `tests` module below, plus
//! `registration_matches_an_existing_index_by_root_path` and
//! `registration_falls_back_to_a_collision_resistant_id` in
//! `search_index_tests.rs`.

use std::path::Path;

use super::{
    IndexOptions, IndexRegistration, best_effort_create_index, registered_root_from_response,
};

/// What one `POST /indexes` achieved, with the conflict told apart (#6864).
///
/// Why: [`IndexRegistration`] collapses "the daemon refused" and "the daemon
/// holds this id, or this tree, under different terms" into `NotConfirmed`, and
/// only the second is recoverable. Separating them here is what lets
/// [`create_and_reconcile`] retry instead of stranding the session unpinned.
/// What: `Confirmed` is a 2xx naming this same tree; `Conflict` is a `409` or a
/// `200 {created: false}` whose `root_path` is another tree, carrying the
/// `existing_id` when the daemon named one; `NotConfirmed` is every other
/// non-2xx, a transport error, and a panicked worker.
/// Test: `a_root_collision_409_carries_the_existing_id`,
/// plus `create_index_response_for_a_different_tree_reports_a_conflict` and
/// `create_index_response_for_the_same_tree_is_confirmed` in
/// `search_index_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CreateOutcome {
    /// The daemon acknowledged the index for the requested tree.
    Confirmed,
    /// The requested id cannot name this tree. `existing_id` is set when the
    /// daemon's `409` named the index that already owns this `root_path`.
    Conflict { existing_id: Option<String> },
    /// Nothing was registered and nothing here can recover it.
    NotConfirmed,
}

/// Read one `POST /indexes` answer as a [`CreateOutcome`].
///
/// Why: the status alone never decided this. A `200 {created: false}` naming
/// another tree is a conflict wearing a success code (#5065 review), and a `409`
/// is the daemon saying the same thing outright — one index identifies one
/// directory tree, so a request naming a tree it does not hold under that id has
/// not been satisfied. Both are recoverable, and the rule that says so lives
/// here, beside the recovery, so the two cannot drift apart.
/// What: `409` yields `Conflict` carrying whatever `existing_id` the body names;
/// a 2xx whose reported `root_path` is another tree yields `Conflict` with none
/// (no index for this tree has been identified yet); any other 2xx is
/// `Confirmed`; every other status is `NotConfirmed`. `root_display` is the
/// requested root as the caller already rendered it, for the log lines.
/// Test: `search_index_tests.rs::{create_index_response_for_a_different_tree_reports_a_conflict,
/// create_index_response_for_the_same_tree_is_confirmed,
/// create_rejected_by_the_daemon_withholds_the_pinnable_id}`.
pub(super) fn classify_create_response(
    status: reqwest::StatusCode,
    body: &str,
    index_id: &str,
    root: &Path,
    root_display: &str,
) -> CreateOutcome {
    if status == reqwest::StatusCode::CONFLICT {
        tracing::warn!(
            "trusty-search index registration for '{index_id}' at {root_display} \
             conflicts (HTTP 409); looking for the index already serving that tree"
        );
        return CreateOutcome::Conflict {
            existing_id: colliding_index_id_from_response(body),
        };
    }
    if !status.is_success() {
        tracing::warn!("trusty-search index registration for '{index_id}' returned HTTP {status}");
        return CreateOutcome::NotConfirmed;
    }
    match registered_root_from_response(body) {
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

/// Find-or-create `index_id` for `root`, resolving a basename collision to the
/// index that already serves that tree (#6864).
///
/// Why: see the module doc. The returned id is what the caller PINS, so it must
/// name an index the daemon actually holds for this root — not the id that was
/// asked for.
/// What: runs [`best_effort_create_index`]; a `Confirmed` or unrecoverable
/// answer returns the requested id unchanged, and a conflict goes through
/// [`resolve_colliding_id`]. Returns the id to pin alongside the registration
/// verdict; `IndexRegistration::Confirmed` is returned only when some index for
/// this root was acknowledged.
/// Test: `search_index_tests.rs::{registration_matches_an_existing_index_by_root_path,
/// registration_falls_back_to_a_collision_resistant_id,
/// create_rejected_by_the_daemon_withholds_the_pinnable_id}`.
pub(super) fn create_and_reconcile(
    base: &str,
    index_id: &str,
    root: &Path,
    opts: IndexOptions,
) -> (String, IndexRegistration) {
    match best_effort_create_index(base, index_id, root, opts) {
        CreateOutcome::Confirmed => (index_id.to_string(), IndexRegistration::Confirmed),
        CreateOutcome::NotConfirmed => (index_id.to_string(), IndexRegistration::NotConfirmed),
        // #6864: the id is taken, or this tree is; ask which index serves it.
        CreateOutcome::Conflict { existing_id } => {
            match resolve_colliding_id(base, index_id, root, opts, existing_id) {
                Some(resolved) => (resolved, IndexRegistration::Confirmed),
                None => (index_id.to_string(), IndexRegistration::NotConfirmed),
            }
        }
    }
}

/// The id of an index that serves `root`, after the derived id failed (#6864).
///
/// Why: three sources answer the same question and they are tried cheapest
/// first. The `409` body already names the index owning this `root_path` when
/// the collision was on the tree, so no request is needed at all. A collision on
/// the ID instead needs the registry read, which is the one extra round trip
/// this fix costs. Only when neither finds an index for this tree is a second
/// create justified — and it uses [`crate::derive_checkout_index_id`], the
/// path-digest form #6149 defined for exactly this, rather than a new scheme.
/// What: returns the id to pin, or `None` when nothing serves this root and the
/// fallback create did not land. Registering under the digest id can itself
/// conflict — a COLD registry entry for this tree is not listed by
/// `GET /indexes?details=true` but is still checked by the daemon's
/// root-collision guard — so that answer's `existing_id` is honoured too.
/// Test: `a_root_collision_409_carries_the_existing_id` covers the body read; the
/// three-tier resolution is the two live-daemon tests named on
/// [`create_and_reconcile`].
fn resolve_colliding_id(
    base: &str,
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

    if let Some(body) = fetch_index_list(base)
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
    match best_effort_create_index(base, &fresh, root, opts) {
        CreateOutcome::Confirmed => Some(fresh),
        CreateOutcome::Conflict { existing_id } => existing_id,
        CreateOutcome::NotConfirmed => None,
    }
}

/// The `existing_id` a `409` names as the current owner of a `root_path`.
///
/// Why: trusty-search answers a create whose `root_path` is already registered
/// with `root_path_collision_response`, which names the owning index precisely
/// so the caller does not have to cross-reference the registry by hand. Reading
/// it turns the most common recoverable conflict into zero extra requests.
/// What: the string `existing_id`, or `None` for any other body — including
/// `root_path_mismatch_response`, the same-id-different-tree `409`, which
/// deliberately carries no such field because no index serves the requested
/// tree.
/// Test: `a_root_collision_409_carries_the_existing_id`,
/// `a_root_mismatch_409_names_no_existing_index`.
pub(super) fn colliding_index_id_from_response(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value.get("existing_id")?.as_str().map(str::to_string)
}

/// The id of the registry entry whose `root_path` IS `root`.
///
/// Why: matching on the id is what created #6864; matching on the tree is what
/// resolves it. [`crate::identifies_same_path`] is the shared `(dev, ino)`
/// comparison the daemon's own collision guard uses, so a case-variant or
/// symlinked spelling of one tree matches here exactly as it does there.
/// What: scans `GET /indexes?details=true`'s `indexes` array and returns the
/// first entry whose `root_path` names the same tree. Entries missing `id` or
/// `root_path` are skipped rather than failing the scan — the daemon omits
/// `root_path` for a non-UTF-8 root, and an entry that cannot be compared is
/// simply not a match.
/// Test: `index_id_serving_root_matches_on_the_tree_not_the_id`,
/// `index_id_serving_root_is_none_when_no_entry_matches`,
/// `index_id_serving_root_tolerates_a_malformed_body`.
pub(super) fn index_id_serving_root(body: &str, root: &Path) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    for entry in value.get("indexes")?.as_array()? {
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
/// Why: `root_path` rides only on `?details=true`; the flat list is bare ids and
/// cannot answer which index serves a tree. This runs on the same hot path as
/// the create it follows, so it carries the create's caps — ~1s overall, 750 ms
/// connect — and its own dedicated OS thread, because `reqwest::blocking` panics
/// when its runtime is dropped inside a tokio worker and both callers of
/// `ensure_project_indexed*` are frequently async.
/// What: `GET {base}/indexes?details=true`, returning the body on a 2xx and
/// `None` for a non-2xx, a transport error, or a panicked worker. A `None` here
/// falls through to the fallback create rather than failing the registration.
/// Test: covered through `registration_matches_an_existing_index_by_root_path`
/// in `search_index_tests.rs`, which serves this request from a fake daemon.
fn fetch_index_list(base: &str) -> Option<String> {
    let url = format!("{}/indexes?details=true", base.trim_end_matches('/'));

    let result = std::thread::spawn(move || {
        // #4392: loopback target, so proxies stay off.
        let client = crate::http_client::blocking_loopback_client_builder()
            .timeout(std::time::Duration::from_secs(1))
            .connect_timeout(std::time::Duration::from_millis(750))
            .build()?;
        let resp = client.get(&url).send()?;
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        Ok::<(reqwest::StatusCode, String), reqwest::Error>((status, text))
    })
    .join();

    match result {
        Ok(Ok((status, body))) if status.is_success() => Some(body),
        Ok(Ok((status, _))) => {
            tracing::warn!("trusty-search index list returned HTTP {status}");
            None
        }
        Ok(Err(e)) => {
            tracing::warn!("trusty-search index list failed: {e}");
            None
        }
        Err(_) => {
            tracing::warn!("trusty-search index list thread panicked");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: this is the zero-request recovery — the daemon already told us which
    /// index owns the tree, so reading it must not require a registry scan.
    /// Test: itself.
    #[test]
    fn a_root_collision_409_carries_the_existing_id() {
        let body =
            r#"{"error":"root_path is already registered","existing_id":"trusty-tools-checkout"}"#;
        assert_eq!(
            colliding_index_id_from_response(body),
            Some("trusty-tools-checkout".to_string())
        );
    }

    /// Why: the same-id-different-tree `409` names the OTHER checkout's root, not
    /// an index serving ours. Reading an id out of it would pin the very index
    /// #6864 is about not pinning.
    /// Test: itself.
    #[test]
    fn a_root_mismatch_409_names_no_existing_index() {
        let body = r#"{"error":"index 'trusty-tools' is registered at ...","index_id":"trusty-tools",
             "registered_root_path":"/Users/masa/Projects/trusty-tools",
             "requested_root_path":"/Users/masa/checkout/trusty-tools"}"#;
        assert_eq!(colliding_index_id_from_response(body), None);
        assert_eq!(colliding_index_id_from_response("not json"), None);
    }

    /// Why: the whole fix in one assertion — the entry that matches is the one
    /// whose ROOT is this tree, even though a different entry carries the id the
    /// basename derives.
    /// Test: itself.
    #[test]
    fn index_id_serving_root_matches_on_the_tree_not_the_id() {
        let mine = std::env::temp_dir();
        let body = format!(
            r#"{{"indexes":[{{"id":"trusty-tools","root_path":"/nonexistent/other/trusty-tools"}},
               {{"id":"trusty-tools-checkout","root_path":"{}"}}]}}"#,
            mine.display()
        );
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
        let body = r#"{"indexes":[{"id":"api","root_path":"/nonexistent/work/api"}]}"#;
        assert_eq!(
            index_id_serving_root(body, Path::new("/nonexistent/work/other")),
            None
        );
    }

    /// Why: this parses a daemon response on a hot path, so every malformed shape
    /// must degrade to "no match" rather than panic. An entry with a null
    /// `root_path` (a non-UTF-8 root) is skipped, not treated as a match.
    /// Test: itself.
    #[test]
    fn index_id_serving_root_tolerates_a_malformed_body() {
        let root = std::env::temp_dir();
        assert_eq!(index_id_serving_root("not json", &root), None);
        assert_eq!(index_id_serving_root("{}", &root), None);
        assert_eq!(index_id_serving_root(r#"{"indexes":"nope"}"#, &root), None);
        assert_eq!(
            index_id_serving_root(r#"{"indexes":[{"id":"x","root_path":null}]}"#, &root),
            None
        );
        assert_eq!(
            index_id_serving_root(
                &format!(r#"{{"indexes":[{{"root_path":"{}"}}]}}"#, root.display()),
                &root
            ),
            None,
            "an entry with no id cannot be pinned"
        );
    }
}
