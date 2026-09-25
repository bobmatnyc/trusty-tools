//! Retract kuzu edges the source no longer carries (#277 MEDIUM-2).
//!
//! Why: owner ruling (a) — under `--update`, a MENTIONS or RELATES_TO edge
//! removed in kuzu must not stay active in the palace, and each retraction is
//! logged with its memory id.
//! What: [`retract_stale`] reads the active triples of each in-scope drawer
//! and closes every kuzu-provenance `mentions` / `relates_to[:<type>]` triple
//! the current export does not name. Triples of any other provenance, and
//! entity triples, are never touched. Without a sink it only counts.
//! Test: `update_retracts_kuzu_edges_absent_from_the_source`,
//! `each_retraction_is_reported_with_its_memory_id`.

use std::collections::HashSet;

use trusty_common::memory_core::store::Triple;
use uuid::Uuid;

use super::apply::{fail, PalaceSink, PalaceView, StoreCounts};
use super::mapping::{drawer_subject, ORIGIN_TAG};

/// One closed edge: the memory whose drawer it hung from, and its predicate
/// family (`mentions` or `relates_to`) — never the object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retraction {
    pub memory_id: String,
    pub family: &'static str,
}

/// The predicate family of a triple this importer writes from an edge.
fn edge_family(t: &Triple) -> Option<&'static str> {
    if t.provenance.as_deref() != Some(ORIGIN_TAG) {
        return None;
    }
    match t.predicate.as_str() {
        "mentions" => Some("mentions"),
        p if p == "relates_to" || p.starts_with("relates_to:") => Some("relates_to"),
        _ => None,
    }
}

/// Close each kuzu edge on `scope`'s drawers that `keep` does not name.
///
/// `scope` is `(memory id, drawer id)`; `keep` holds `(subject, predicate,
/// object)` of every edge the export still names. A failed read or retract is
/// counted in `failed_writes`, and the walk continues.
pub async fn retract_stale(
    scope: &[(String, Uuid)],
    keep: &HashSet<(String, String, String)>,
    view: &dyn PalaceView,
    sink: Option<&dyn PalaceSink>,
    c: &mut StoreCounts,
) {
    for (memory_id, id) in scope {
        let active = match view.active_triples(&drawer_subject(*id)).await {
            Ok(a) => a,
            Err(e) => {
                fail(c, "read triples", &e);
                continue;
            }
        };
        for t in active {
            let Some(family) = edge_family(&t) else {
                continue;
            };
            if keep.contains(&(t.subject.clone(), t.predicate.clone(), t.object.clone())) {
                continue;
            }
            if let Some(s) = sink {
                if let Err(e) = s.retract_triple(&t).await {
                    fail(c, "retract triple", &e);
                    continue;
                }
            }
            tracing::info!(%memory_id, family, "kuzu import: retracted an edge the source no longer has");
            c.retracted.push(Retraction {
                memory_id: memory_id.clone(),
                family,
            });
        }
    }
}
