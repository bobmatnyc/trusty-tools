//! The retirement marker, end to end through the MCP dispatcher (#10609,
//! ADR-0074, owner ruling D83).
//!
//! Why: the Tier C retirement commit stamps `superseded-by:<id>` on the retiree
//! so default recall excludes it with no KG lookup. The first cut stamped only
//! the redb row; every recall lane reads the in-memory mirror, so a running
//! daemon kept serving the retiree until the palace reopened, and the existing
//! end-to-end test passed only because the `superseded_by` edge excluded it too.
//! What: forces the edge to be absent (a poisoned adjacency refuses the edge
//! write, as in `a_failed_edge_write_still_lands_the_remember_and_warns`) or
//! unreadable (an undecodable `superseded_by` row), so the marker alone is
//! under test; plus the reserved-namespace guard on caller-supplied tags.
//! Test: this IS the test module.

use super::*;
use crate::engine::palace::RoomType;
use crate::engine::retrieval::{PalaceHandle, RememberOptions};
use crate::engine::share::SUPERSEDED_BY;
use crate::engine::store::kg::{Triple, tests::poison_adjacency};
use crate::engine::store::kg_store::{TRIPLES, encode_triple_key};
use chrono::Utc;
use std::sync::Arc;

const SLOT: &str = "pr:10609/state";
const QUERY: &str = "pull request 10609 merge state";
const OLD: &str = "pull request 10609 merge state: in flight, resume from branch fix-a";
const NEW: &str = "pull request 10609 merge state: merged, nothing to resume";

async fn palace(state: &AppState, name: &str) -> Arc<PalaceHandle> {
    dispatch_tool(state, "palace_create", json!({ "name": name }))
        .await
        .expect("palace_create");
    crate::tools::open_palace_handle(state, name).expect("palace handle")
}

async fn write_slot(handle: &PalaceHandle, text: &str) -> Uuid {
    handle
        .remember_with_options(
            text.to_string(),
            RoomType::General,
            vec![],
            0.5,
            RememberOptions {
                fact_key: Some(SLOT.to_string()),
                ..RememberOptions::forced()
            },
        )
        .await
        .expect("slot write")
}

async fn recall(state: &AppState, palace: &str, include_superseded: bool) -> Value {
    dispatch_tool(
        state,
        "memory_recall",
        json!({
            "palace": palace,
            "query": QUERY,
            "top_k": 10,
            "include_superseded": include_superseded,
        }),
    )
    .await
    .expect("memory_recall")
}

fn ids(out: &Value) -> Vec<String> {
    out["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|r| r["drawer_id"].as_str().expect("drawer_id").to_string())
        .collect()
}

/// The drawer as the in-memory mirror holds it.
fn mirrored(handle: &PalaceHandle, id: Uuid) -> crate::engine::palace::Drawer {
    handle
        .drawers
        .read()
        .iter()
        .find(|d| d.id == id)
        .cloned()
        .expect("drawer in the mirror")
}

/// Why (#10609 review, HIGH): the marker must reach the mirror, because every
/// recall lane reads it; with the edge write refused it is the only signal.
/// What: a slot write whose `superseded_by` edge write fails. The mirror's
/// retiree carries the marker, and default `memory_recall` leaves it out while
/// returning its replacement. The historical opt-in brings it back, so the
/// absence is exclusion and not a miss. Fails on 495a286a4e, where the mirror
/// copy had no marker.
/// Test: this test.
#[tokio::test]
async fn a_retiree_is_marked_in_the_mirror_and_hidden_when_the_edge_write_fails() {
    let (state, _tmp) = test_state();
    let handle = palace(&state, "rm1").await;
    let old = write_slot(&handle, OLD).await;
    poison_adjacency(&handle.kg);
    let refused = handle
        .kg
        .assert(Triple {
            subject: "probe".into(),
            predicate: "marks".into(),
            object: "desync".into(),
            valid_from: Utc::now(),
            valid_to: None,
            confidence: 1.0,
            provenance: None,
        })
        .await;
    assert!(refused.is_err() && handle.kg.adjacency_desynced());

    let new = write_slot(&handle, NEW).await;

    let edges = handle.kg.superseded_by_many(vec![old, new]).await.unwrap();
    assert!(
        edges.is_empty(),
        "the edge write must have failed: {edges:?}"
    );
    assert_eq!(mirrored(&handle, old).retired_by(), Some(new));
    assert_eq!(mirrored(&handle, new).retired_by(), None);

    let default = ids(&recall(&state, "rm1", false).await);
    assert!(default.contains(&new.to_string()), "{default:?}");
    assert!(!default.contains(&old.to_string()), "leaked: {default:?}");

    let historical = recall(&state, "rm1", true).await;
    let row = historical["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["drawer_id"] == json!(old.to_string()))
        .expect("the opt-in returns the retiree");
    assert_eq!(row["superseded_by"], json!(new.to_string()), "{row:#}");
}

/// Why (#10609): a recall whose edge lookup errors cannot check edge-only
/// supersessions, and must say so on the wire; the marker still excludes a slot
/// retiree. `memory_list` hides the internal marker tag.
/// What: a slot pair, then an undecodable `superseded_by` row under the new
/// drawer, so `superseded_by_many` errors. The `memory_recall` response carries
/// `supersession_unverified: true` and still omits the retiree; before the bad
/// row the key is absent. `memory_list` shows the retiree with no marker tag
/// although the mirror holds one.
/// Test: this test.
#[tokio::test]
async fn a_failed_edge_lookup_flags_the_recall_and_the_marker_still_excludes() {
    let (state, _tmp) = test_state();
    let handle = palace(&state, "rm2").await;
    let old = write_slot(&handle, OLD).await;
    let new = write_slot(&handle, NEW).await;

    let healthy = recall(&state, "rm2", false).await;
    assert!(
        healthy.get("supersession_unverified").is_none(),
        "{healthy:#}"
    );

    let db = handle.kg.redb_store().db_for_test();
    let wtx = db.begin_write().expect("begin");
    {
        let mut t = wtx.open_table(TRIPLES).expect("open triples");
        let key = encode_triple_key(&format!("drawer:{new}"), SUPERSEDED_BY, "drawer:garbage");
        t.insert(key.as_slice(), b"not a postcard TripleValue".as_slice())
            .expect("insert");
    }
    wtx.commit().expect("commit");

    let out = recall(&state, "rm2", false).await;
    assert_eq!(out["supersession_unverified"], json!(true), "{out:#}");
    let shown = ids(&out);
    assert!(shown.contains(&new.to_string()), "{shown:?}");
    assert!(!shown.contains(&old.to_string()), "leaked: {shown:?}");

    assert!(mirrored(&handle, old).retired_by().is_some());
    let listed = dispatch_tool(
        &state,
        "memory_list",
        json!({ "palace": "rm2", "limit": 50 }),
    )
    .await
    .expect("memory_list");
    let row = listed["drawers"]
        .as_array()
        .expect("drawers")
        .iter()
        .find(|d| d["drawer_id"] == json!(old.to_string()))
        .expect("memory_list shows the retiree");
    let tags = row["tags"].as_array().expect("tags");
    assert!(
        tags.iter()
            .all(|t| !t.as_str().unwrap_or("").starts_with("superseded-by:")),
        "{tags:?}"
    );
}

/// Why (#10609 review): `superseded-by:` is reserved to the retirement commit.
/// A caller-supplied one (or one copied from a recalled row) would hide a live
/// drawer. The write strips it, with a WARN, rather than failing the write.
/// What: `memory_remember` with a marker tag plus an ordinary tag; the drawer is
/// stored with the ordinary tag only and is returned by default recall.
/// Test: this test.
#[tokio::test]
async fn a_caller_supplied_marker_tag_is_stripped_and_hides_nothing() {
    let (state, _tmp) = test_state();
    let handle = palace(&state, "rm3").await;
    let out = dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "rm3",
            "text": NEW,
            "tags": [format!("superseded-by:{}", Uuid::new_v4()), "keep"],
            "force": true,
        }),
    )
    .await
    .expect("memory_remember");
    let id = Uuid::parse_str(out["drawer_id"].as_str().expect("drawer_id")).expect("uuid");

    let stored = mirrored(&handle, id);
    assert_eq!(stored.retired_by(), None);
    assert!(
        stored.tags.contains(&"keep".to_string()),
        "{:?}",
        stored.tags
    );
    assert!(stored.tags.iter().all(|t| !t.starts_with("superseded-by:")));
    assert!(ids(&recall(&state, "rm3", false).await).contains(&id.to_string()));
}
