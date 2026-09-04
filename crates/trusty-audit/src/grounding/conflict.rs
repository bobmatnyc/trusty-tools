//! Resolving a `409 Conflict` from `POST /indexes` instead of skipping it (#6783).
//!
//! Why: `trusty-search index <path> --name <id>` registers the checkout before
//! it indexes it, and the daemon refuses that registration with `409` in two
//! cases — the id is already registered at a DIFFERENT root, or this root is
//! already registered under a DIFFERENT id. Both are stale rows left by an
//! earlier run on the same machine, and #6149 made the second one routine: it
//! changed the id derivation, so every repository an older release registered
//! under its checkout basename now collides with the hashed id this release
//! derives for the same tree. A client sweep of 59 repositories hit it 59 times.
//! [`super::index::ensure_indexed`] reported the refusal and [`super::ground`]
//! turned it into a gap, so the run produced 59 reports with no search evidence,
//! no complexity, no health factors, and no finding verified against a symbol —
//! over a clean exit and a "59 of 59" index. A stale row is recoverable, and a
//! recoverable failure that costs a whole evidence tier must be recovered.
//!
//! ## The policy
//!
//! Read the registry, decide, act, retry the create ONCE:
//!
//! | What the registry says | What happens |
//! |---|---|
//! | `<id>` is registered at this checkout | Reuse it. Nothing is deleted. |
//! | `<id>` is registered at another root | Deregister `<id>`, then re-create. |
//! | This checkout is registered under another id | Deregister that id, then re-create under `<id>`. |
//! | Neither — the registry does not explain the refusal | Report it. |
//!
//! Deregistration NEVER destroys data (`delete_data` stays absent, which the
//! daemon reads as `false`), and it is guarded by `expected_root_path`, so a
//! registration that changed between the read and the write is refused rather
//! than deleted on stale information. The corpus survives; only the row goes.
//!
//! Re-creating under `<id>` rather than adopting the id already registered is
//! deliberate: the id is a cross-process contract — `trusty_review`'s renderer
//! and `tga`'s sweep both derive it independently through
//! [`trusty_common::derive_checkout_index_id`] — so an index registered under
//! any other name is one nobody looks up. See [`super::index`]'s module docs.
//!
//! Test: `conflict_tests`.

use std::path::Path;
use std::time::Duration;

use super::search_rpc;

/// Wall-clock budget for each registry call made while resolving a conflict.
const BUDGET: Duration = Duration::from_secs(10);

/// One row of the daemon's index registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// The index id the daemon serves this row under.
    pub id: String,
    /// The tree it is rooted at, as the daemon spells it. `None` for a row whose
    /// root is not valid UTF-8, which cannot be compared or guarded on.
    pub root_path: Option<String>,
}

/// What the registry says should be done about the refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The wanted id already names this checkout; the create was redundant.
    Reuse,
    /// One stale row stands between this checkout and its id — drop it.
    Drop {
        /// The registration to deregister.
        id: String,
        /// The root it currently claims, sent as the delete's guard.
        root: String,
    },
    /// Nothing in the registry accounts for the refusal.
    Unexplained,
}

/// What resolving the conflict actually achieved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// The daemon already serves this checkout under this id — nothing to redo.
    Reuse,
    /// A stale registration was dropped; the create is worth retrying.
    Dropped {
        /// The id that was deregistered.
        id: String,
    },
}

/// True when a failed `trusty-search index` says the daemon refused with `409`.
///
/// Why: the CLI reports the refusal as one stderr line and exits non-zero, so
/// the status code is all that distinguishes a recoverable registration
/// collision from every other reason indexing fails — an unapproved root, a dead
/// embedder, a full disk. Only the collision is worth a second attempt.
/// What: matches on the code and a second token, so a `409` occurring inside a
/// checkout path is not mistaken for a status.
/// Test: `conflict_tests::{a_409_from_the_create_route_is_a_registration_conflict,
/// an_unrelated_failure_is_not_a_registration_conflict}`.
#[must_use]
pub fn is_registration_conflict(reason: &str) -> bool {
    reason.contains("409")
        && (reason.contains("Conflict")
            || reason.contains("conflict")
            || reason.contains("indexes"))
}

/// Read `{"indexes": [...]}` as registry rows, dropping anything unreadable.
///
/// The daemon answers `search.indexes.list` with bare id strings by default and
/// with objects under `{"details": true}`; both shapes are accepted so a
/// response that lost its detail fields still yields ids.
fn registrations(body: &serde_json::Value) -> Vec<Registration> {
    let Some(rows) = body.get("indexes").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| match row {
            serde_json::Value::String(id) => Some(Registration {
                id: id.clone(),
                root_path: None,
            }),
            other => Some(Registration {
                id: other.get("id")?.as_str()?.to_owned(),
                root_path: other
                    .get("root_path")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
            }),
        })
        .collect()
}

/// The policy table in the module docs, as a pure function.
///
/// Why pure and separate from the calls that carry it out: the decision is the
/// part worth asserting, and asserting it against a live daemon would make the
/// table's arms untestable without one.
/// What: the wanted id wins — a row under `index_id` decides the outcome by
/// itself, and only when there is none does a foreign row owning this checkout
/// matter. A row whose root is unreadable cannot be compared OR guarded on, so
/// it never produces a `Drop`.
/// Test: `conflict_tests::{the_wanted_id_at_this_checkout_is_reused,
/// the_wanted_id_at_another_root_is_dropped,
/// this_checkout_under_a_foreign_id_drops_the_foreign_row,
/// a_registry_that_explains_nothing_is_unexplained}`.
#[must_use]
pub fn decide(rows: &[Registration], index_id: &str, checkout: &Path) -> Decision {
    if let Some(mine) = rows.iter().find(|r| r.id == index_id) {
        let Some(root) = &mine.root_path else {
            return Decision::Unexplained;
        };
        if super::index::same_tree(Path::new(root), checkout) {
            return Decision::Reuse;
        }
        return Decision::Drop {
            id: mine.id.clone(),
            root: root.clone(),
        };
    }
    for row in rows {
        let Some(root) = &row.root_path else { continue };
        if super::index::same_tree(Path::new(root), checkout) {
            return Decision::Drop {
                id: row.id.clone(),
                root: root.clone(),
            };
        }
    }
    Decision::Unexplained
}

/// Ask the daemon what it is serving, decide, and clear the stale row.
///
/// # Errors
///
/// One line, safe to show the recipient, when the registry cannot be read, when
/// it does not account for the refusal, or when the daemon refuses the delete.
/// The caller reports it beside the original refusal; nothing here fails a run.
///
/// # Postconditions
/// Never panics. On [`Resolution::Dropped`] exactly one registration was
/// deregistered and its data was left on disk.
///
/// Test: `super::grounding_tests::a_409_registration_conflict_clears_the_stale_row_and_retries`,
/// `super::grounding_tests::an_unresolvable_409_degrades_the_evidence_tier_out_loud`.
pub async fn resolve(socket: &Path, index_id: &str, checkout: &Path) -> Result<Resolution, String> {
    let listed = search_rpc::call(
        socket,
        search_rpc::METHOD_INDEXES_LIST,
        serde_json::json!({ "details": true }),
        BUDGET,
    )
    .await
    .map_err(|e| e.to_string())?;
    match decide(&registrations(&listed), index_id, checkout) {
        Decision::Reuse => Ok(Resolution::Reuse),
        Decision::Unexplained => Err(format!(
            "trusty-search refused the registration of '{index_id}' but serves no index at {} \
             and none named '{index_id}' — the collision is not a stale registration this run \
             can clear",
            checkout.display()
        )),
        Decision::Drop { id, root } => {
            // #6783: `delete_data` is deliberately absent — the daemon reads it
            // as false, so the corpus survives and only the row goes.
            search_rpc::call(
                socket,
                search_rpc::METHOD_INDEX_DELETE,
                serde_json::json!({ "index_id": id, "expected_root_path": root }),
                BUDGET,
            )
            .await
            .map_err(|e| {
                format!("the stale registration '{id}' at {root} could not be dropped: {e}")
            })?;
            Ok(Resolution::Dropped { id })
        }
    }
}

#[cfg(test)]
mod conflict_tests {
    use super::*;

    fn row(id: &str, root: &str) -> Registration {
        Registration {
            id: id.to_owned(),
            root_path: Some(root.to_owned()),
        }
    }

    /// The exact line the client run produced 59 times (#6783).
    #[test]
    fn a_409_from_the_create_route_is_a_registration_conflict() {
        let line = "`trusty-search index /w/repos/xsv --name xsv-9f2a` failed (exit status: 1): \
                    daemon returned 409 Conflict for POST /indexes";
        assert!(is_registration_conflict(line), "{line}");
    }

    #[test]
    fn an_unrelated_failure_is_not_a_registration_conflict() {
        for line in [
            "`trusty-search index /w/repos/xsv --name xsv` failed (exit status: 1): \
             indexing refused: root is not allowlisted",
            "`trusty-search index /w/409-experiments/xsv --name xsv` failed (exit status: 1): \
             embedder initializing",
        ] {
            assert!(!is_registration_conflict(line), "{line}");
        }
    }

    #[test]
    fn the_wanted_id_at_this_checkout_is_reused() {
        let rows = vec![row("xsv-9f2a", "/w/repos/xsv"), row("other", "/w/repos/y")];
        assert_eq!(
            decide(&rows, "xsv-9f2a", Path::new("/w/repos/xsv")),
            Decision::Reuse
        );
    }

    #[test]
    fn the_wanted_id_at_another_root_is_dropped() {
        let rows = vec![row("xsv-9f2a", "/w/repos/other-xsv")];
        assert_eq!(
            decide(&rows, "xsv-9f2a", Path::new("/w/repos/xsv")),
            Decision::Drop {
                id: "xsv-9f2a".to_owned(),
                root: "/w/repos/other-xsv".to_owned(),
            }
        );
    }

    /// #6149 changed the id derivation, so an older run's basename row owns the
    /// tree this run wants under a hashed id. That row is what must go.
    #[test]
    fn this_checkout_under_a_foreign_id_drops_the_foreign_row() {
        let rows = vec![row("unrelated", "/w/repos/y"), row("xsv", "/w/repos/xsv")];
        assert_eq!(
            decide(&rows, "xsv-9f2a", Path::new("/w/repos/xsv")),
            Decision::Drop {
                id: "xsv".to_owned(),
                root: "/w/repos/xsv".to_owned(),
            }
        );
    }

    #[test]
    fn a_registry_that_explains_nothing_is_unexplained() {
        let rows = vec![row("unrelated", "/w/repos/y")];
        assert_eq!(
            decide(&rows, "xsv-9f2a", Path::new("/w/repos/xsv")),
            Decision::Unexplained
        );
        assert_eq!(
            decide(&[], "xsv-9f2a", Path::new("/w/repos/xsv")),
            Decision::Unexplained
        );
    }

    /// A row with no readable root can be neither compared nor guarded on, so it
    /// must never be deleted on a guess.
    #[test]
    fn a_row_with_no_readable_root_is_never_dropped() {
        let rows = vec![Registration {
            id: "xsv-9f2a".to_owned(),
            root_path: None,
        }];
        assert_eq!(
            decide(&rows, "xsv-9f2a", Path::new("/w/repos/xsv")),
            Decision::Unexplained
        );
    }

    #[test]
    fn both_listing_shapes_parse() {
        let detailed = serde_json::json!({
            "indexes": [{"id": "xsv", "root_path": "/w/repos/xsv", "size_bytes": 12}]
        });
        assert_eq!(registrations(&detailed), vec![row("xsv", "/w/repos/xsv")]);

        let flat = serde_json::json!({ "indexes": ["xsv", "acme"] });
        let parsed = registrations(&flat);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].id, "xsv");
        assert_eq!(parsed[0].root_path, None);

        assert!(registrations(&serde_json::json!({})).is_empty());
    }
}
