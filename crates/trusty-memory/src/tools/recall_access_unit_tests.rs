//! #9881: which served hits count as an access (rulings Q2 and Q3).

use super::*;
use crate::engine::palace::Drawer;

fn hit(layer: u8) -> RecallResult {
    RecallResult {
        drawer: Drawer::new(Uuid::new_v4(), "served"),
        score: 0.5,
        layer,
    }
}

/// Why: L0 and L1 come back on every recall whatever the query, so counting
/// them would inflate the essentials (Q2); a folded ruling was found by search
/// in its own palace, so it counts there (Q3).
/// What: one hit per layer 0-3, a ruling relabelled layer 1 with an owner,
/// and an owner entry with no palace (a fixture fold). Lane hits are layers
/// 2 and 3; the owned ruling goes to its palace; the rest are dropped.
/// Test: this function.
#[test]
fn served_hits_count_lane_and_ruling_hits_only() {
    let results: Vec<RecallResult> = [0, 1, 2, 3, 1, 1].into_iter().map(hit).collect();
    let ids: Vec<Uuid> = results.iter().map(|r| r.drawer.id).collect();
    let owners = HashMap::from([(ids[4], "rulings".to_string()), (ids[5], String::new())]);
    let served = ServedHits::from_cut(&results, &owners);
    assert_eq!(
        served,
        ServedHits {
            lane: vec![ids[2], ids[3]],
            rulings: vec![("rulings".to_string(), ids[4])],
        }
    );
}
