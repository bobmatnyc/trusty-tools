//! Tests for the batch `superseded_by` lookup recall demotion reads (#9421).
//!
//! Why: recall trusts this map to decide which drawers to demote, so a wrong
//! entry demotes a current drawer and a missing one leaves a stale drawer on
//! top. The opt-in profile measures what the lookup adds to a recall, for the
//! E12 latency budget (#9280: the current path plus 5 ms at 20k rows).
//! What: one behaviour test over every edge shape the store can hold, an
//! empty-input test, and `kg_supersession_latency_profile`, a no-op unless
//! `TRUSTY_KG_SUPERSESSION_PROFILE` is set. Run the profile in release:
//! `TRUSTY_KG_SUPERSESSION_PROFILE=1 cargo test -p trusty-common --release
//! --features memory-core --lib kg_supersession_latency_profile -- --nocapture`.
//! Test: this IS the test module.

use std::time::{Duration, Instant};

use chrono::Utc;
use tempfile::tempdir;
use uuid::Uuid;

use super::graph::KnowledgeGraph;
use super::types::Triple;
use crate::memory_core::share::{SUPERSEDED_BY, assert_superseded_by};

fn triple(subject: String, predicate: &str, object: String) -> Triple {
    Triple {
        subject,
        predicate: predicate.to_string(),
        object,
        valid_from: Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: None,
    }
}

/// Why (#9421): only a live edge to another drawer may demote a drawer.
/// What: a written edge maps; a re-pointed edge maps to its newest object; a
/// retracted edge, an edge to a non-drawer object, a self-edge, and a drawer
/// with no edge are all absent.
#[tokio::test]
async fn superseded_by_many_maps_only_active_drawer_edges() {
    let dir = tempdir().expect("tempdir");
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).expect("open kg");
    let [
        old,
        new,
        repointed,
        first,
        second,
        retracted,
        note,
        looped,
        plain,
    ] = std::array::from_fn::<Uuid, 9, _>(|_| Uuid::new_v4());

    assert_superseded_by(&kg, old, new, "test")
        .await
        .expect("edge");
    assert_superseded_by(&kg, repointed, first, "test")
        .await
        .expect("edge");
    // `superseded_by` is functional: the second object closes the first.
    assert_superseded_by(&kg, repointed, second, "test")
        .await
        .expect("edge");
    assert_superseded_by(&kg, retracted, new, "test")
        .await
        .expect("edge");
    kg.retract(&format!("drawer:{retracted}"), SUPERSEDED_BY)
        .await
        .expect("retract");
    kg.assert(triple(
        format!("drawer:{note}"),
        SUPERSEDED_BY,
        "note:a-later-decision".into(),
    ))
    .await
    .expect("non-drawer edge");
    assert_superseded_by(&kg, looped, looped, "test")
        .await
        .expect("self edge");

    let ids = vec![old, new, repointed, retracted, note, looped, plain];
    let map = kg.superseded_by_many(ids).await.expect("lookup");
    assert_eq!(map.get(&old), Some(&new), "{map:?}");
    assert_eq!(map.get(&repointed), Some(&second), "{map:?}");
    assert_eq!(map.len(), 2, "only the two live drawer edges map: {map:?}");
}

/// An empty window asks nothing and answers an empty map.
#[tokio::test]
async fn superseded_by_many_of_no_ids_is_empty() {
    let dir = tempdir().expect("tempdir");
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).expect("open kg");
    let map = kg.superseded_by_many(Vec::new()).await.expect("lookup");
    assert!(map.is_empty());
}

/// Opt-in switch; seeding 20k drawers takes seconds in a debug build.
const PROFILE_ENV: &str = "TRUSTY_KG_SUPERSESSION_PROFILE";
/// Drawers in the profiled palace (the E12 budget is stated at 20k rows).
const PROFILE_DRAWERS: usize = 20_000;
/// Ids per lookup: `ranking_window` at the default `top_k = 10`.
const WINDOW: usize = 20;
const QUERIES: usize = 300;

/// `(p50, p95)` of `samples`, in microseconds.
fn percentiles(samples: &mut [Duration]) -> (u128, u128) {
    samples.sort();
    let at = |q: f64| samples[((samples.len() as f64 * q).ceil() as usize).saturating_sub(1)];
    (at(0.50).as_micros(), at(0.95).as_micros())
}

/// See the module header. Asserts nothing about timing.
#[tokio::test(flavor = "multi_thread")]
async fn kg_supersession_latency_profile() {
    if std::env::var_os(PROFILE_ENV).is_none() {
        return;
    }
    let dir = tempdir().expect("tempdir");
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).expect("open kg");
    let ids: Vec<Uuid> = (0..PROFILE_DRAWERS).map(|_| Uuid::new_v4()).collect();
    // Four triples per drawer, about the live trusty-tools palace's density
    // (93k active triples); one drawer in 50 superseded by the next one.
    let mut rows = Vec::with_capacity(PROFILE_DRAWERS * 4);
    for (n, id) in ids.iter().enumerate() {
        let s = format!("drawer:{id}");
        rows.push(triple(s.clone(), "in_room", format!("room:r{}", n % 7)));
        rows.push(triple(
            s.clone(),
            "mentions",
            format!("entity:e{}", n % 911),
        ));
        rows.push(triple(format!("tag:t{}", n % 31), "tags", s.clone()));
        if n % 50 == 0 && n + 1 < ids.len() {
            rows.push(triple(s, SUPERSEDED_BY, format!("drawer:{}", ids[n + 1])));
        }
    }
    let seeded = rows.len();
    kg.store().import_all(rows, Vec::new()).expect("seed");

    let window = |q: usize| -> Vec<Uuid> {
        (0..WINDOW)
            .map(|i| ids[(q * 7_919 + i * 104_729) % ids.len()])
            .collect()
    };
    for q in 0..20 {
        kg.superseded_by_many(window(q)).await.expect("warm-up");
    }
    let mut samples = Vec::with_capacity(QUERIES);
    let mut found = 0usize;
    for q in 0..QUERIES {
        let start = Instant::now();
        let map = kg.superseded_by_many(window(q)).await.expect("lookup");
        samples.push(start.elapsed());
        found += map.len();
    }
    let (p50, p95) = percentiles(&mut samples);
    println!(
        "drawers={PROFILE_DRAWERS} triples={seeded} window={WINDOW} queries={QUERIES} \
         superseded_found={found} lookup p50/p95 us = {p50}/{p95}"
    );
}
