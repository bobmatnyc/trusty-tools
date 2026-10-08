//! Unit tests for the single-palace recall tail in `tools::recall_ops`.

use super::*;
use trusty_common::memory_core::palace::Drawer;
use uuid::Uuid;

fn hit(content: &str, tags: &[&str], score: f32, layer: u8) -> RecallResult {
    let mut drawer = Drawer::new(Uuid::new_v4(), content);
    drawer.tags = tags.iter().map(|t| t.to_string()).collect();
    RecallResult {
        drawer,
        score,
        layer,
    }
}

/// Why (#9421 review): the rulings floor lifts a ruling that answers the
/// query to rank 3. A superseded ruling can answer it while its replacement,
/// worded differently, does not; lifting the stale one would put it above
/// the ruling that replaced it.
/// What: four strong project hits, a replacement ruling that is not floored
/// and the superseded ruling it replaced, floored and joined to it by an edge
/// the rulings palace reported. The replacement ranks above the old ruling.
#[test]
fn the_rulings_floor_never_lifts_a_superseded_ruling_above_its_replacement() {
    let mut results: Vec<RecallResult> = (0..4)
        .map(|n| hit(&format!("project {n}"), &[], 0.9 - 0.05 * n as f32, 2))
        .collect();
    let new = hit("four builders at once", &["ruling"], 0.30, 1);
    let old = hit(
        "maximum concurrent rust builders is six",
        &["ruling"],
        0.70,
        1,
    );
    let (new_id, old_id) = (new.drawer.id, old.drawer.id);
    results.extend([new, old]);
    let fold = RulingsFold {
        degraded: Vec::new(),
        floored: vec![old_id],
        superseded: Supersessions::from([(old_id, new_id)]),
    };
    let out = RecallCut::new(6, None, false).rank_and_serialize(
        "project-a",
        "maximum concurrent rust builders",
        results,
        fold,
        Supersessions::new(),
    );
    let ids: Vec<&str> = out["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|r| r["drawer_id"].as_str().expect("drawer_id"))
        .collect();
    let rank = |id: Uuid| ids.iter().position(|i| *i == id.to_string());
    let (n, o) = (rank(new_id), rank(old_id));
    assert!(n.is_some() && o.is_some(), "{out:#}");
    assert!(
        n < o,
        "the replacement must rank above the old ruling: {out:#}"
    );
}
