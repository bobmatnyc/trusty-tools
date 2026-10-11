//! `RoutingMode::apply` selection tests (#112), split from `tests_health.rs`
//! to keep that file under the line cap.

use super::routing::RoutingMode;
use crate::core::registry::IndexId;

/// Issue #112: `RoutingMode::All` keeps every index and surfaces the
/// cosine-similarity weight verbatim. Indexes without a weight entry
/// fall back to 1.0.
#[test]
#[serial_test::parallel]
fn routing_mode_all_preserves_every_index_with_weights() {
    let ids = vec![IndexId::new("a"), IndexId::new("b"), IndexId::new("c")];
    let weights: std::collections::HashMap<IndexId, f32> = [
        (IndexId::new("a"), 0.9_f32),
        (IndexId::new("b"), 0.2),
        // "c" deliberately absent → falls back to 1.0
    ]
    .into_iter()
    .collect();

    let (active, map) = RoutingMode::All.apply(&ids, &weights);
    assert_eq!(active.len(), 3, "all routing keeps every index");
    assert!((map.get(&IndexId::new("a")).copied().unwrap() - 0.9).abs() < 1e-6);
    assert!((map.get(&IndexId::new("b")).copied().unwrap() - 0.2).abs() < 1e-6);
    assert!((map.get(&IndexId::new("c")).copied().unwrap() - 1.0).abs() < 1e-6);
}

/// Issue #112: `RoutingMode::TopN` keeps only the N highest-similarity
/// indexes (ranked desc) and zeroes weights to 1.0 — selection has
/// already absorbed relevance.
#[test]
#[serial_test::parallel]
fn routing_mode_top_n_keeps_only_highest_similarity() {
    let ids = vec![IndexId::new("low"), IndexId::new("hi"), IndexId::new("mid")];
    let weights: std::collections::HashMap<IndexId, f32> = [
        (IndexId::new("low"), 0.1_f32),
        (IndexId::new("hi"), 0.95),
        (IndexId::new("mid"), 0.5),
    ]
    .into_iter()
    .collect();

    let (active, map) = RoutingMode::TopN(2).apply(&ids, &weights);
    assert_eq!(active.len(), 2);
    let active_set: std::collections::HashSet<&str> =
        active.iter().map(|id| id.0.as_str()).collect();
    assert!(active_set.contains("hi"));
    assert!(active_set.contains("mid"));
    assert!(!active_set.contains("low"));
    // Selected entries normalised to weight 1.0.
    assert!((map.get(&IndexId::new("hi")).copied().unwrap() - 1.0).abs() < 1e-6);
    assert!((map.get(&IndexId::new("mid")).copied().unwrap() - 1.0).abs() < 1e-6);
    assert!(!map.contains_key(&IndexId::new("low")));
}

/// Issue #112: `RoutingMode::Threshold` drops anything strictly below
/// the threshold and keeps entries at/above it.
#[test]
#[serial_test::parallel]
fn routing_mode_threshold_drops_below_cutoff() {
    let ids = vec![IndexId::new("a"), IndexId::new("b"), IndexId::new("c")];
    let weights: std::collections::HashMap<IndexId, f32> = [
        (IndexId::new("a"), 0.1_f32),
        (IndexId::new("b"), 0.5),
        (IndexId::new("c"), 0.8),
    ]
    .into_iter()
    .collect();

    let (active, map) = RoutingMode::Threshold(0.4).apply(&ids, &weights);
    let active_set: std::collections::HashSet<&str> =
        active.iter().map(|id| id.0.as_str()).collect();
    assert!(!active_set.contains("a"), "0.1 < 0.4 must drop");
    assert!(active_set.contains("b"), "0.5 >= 0.4 must keep");
    assert!(active_set.contains("c"));
    assert!(!map.contains_key(&IndexId::new("a")));
}

/// Indexes missing a weight entry default to neutral 1.0, so threshold
/// routing must not silently drop them — otherwise "no metadata"
/// becomes "no relevance" by accident.
#[test]
#[serial_test::parallel]
fn routing_threshold_keeps_neutral_indexes() {
    let ids = vec![IndexId::new("known"), IndexId::new("missing")];
    let weights: std::collections::HashMap<IndexId, f32> =
        [(IndexId::new("known"), 0.05_f32)].into_iter().collect();

    let (active, _map) = RoutingMode::Threshold(0.5).apply(&ids, &weights);
    let active_set: std::collections::HashSet<&str> =
        active.iter().map(|id| id.0.as_str()).collect();
    assert!(!active_set.contains("known"), "0.05 < 0.5 dropped");
    // Missing entries default to 1.0 → kept.
    assert!(
        active_set.contains("missing"),
        "indexes without a context embedding must use neutral 1.0 weight"
    );
}
