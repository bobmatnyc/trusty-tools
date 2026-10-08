//! End-to-end supersession demotion (#9421): a drawer superseded through a
//! `superseded_by` edge, with no snapshot tag and no `fact_key`, ranks below
//! the drawer that replaced it on every recall surface.
//!
//! Why: `tools::recall_supersede_tests` proves the ranking rule; these prove
//! each recall surface reads the edge from the palace KG and applies it.
//! What: drawers written through `memory_remember`, the edge written through
//! trusty-common's `assert_superseded_by` (the dream and share writer), then
//! recalled, from the project palace and from a user-scope rulings palace.
//! Its own binary, so seeding the mock embedder cannot race the lib tests'
//! real singleton. The fail-open arm (KG read error or timeout) is unit-tested
//! in `tools::recall_supersede_tests`. `recall_supersession_latency_profile`
//! is an opt-in end-to-end timing run for #9421 AC5.
//! Test: this IS the test module.

mod recall_support;

use recall_support::{rank_of, recall, recall_on, remember, state_with, supersede, Surface};
use serde_json::Value;
use uuid::Uuid;

/// Why (#9421 AC1, the eval's S2/S3/S5/S8 shape): the stale drawer repeats the
/// query's wording, so similarity alone puts it first. One pair is untagged,
/// the other carries `ruling` on both sides, as four of the live cases did.
/// What: on all five surfaces, the replacement is recalled and the old drawer
/// ranks below it, or drops out of the cut.
#[tokio::test]
async fn a_superseded_drawer_ranks_below_its_replacement_on_every_recall_surface() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with(&tmp, &["project-a"], &[]).await;
    let pairs = [
        (
            "release train deploy window",
            "the release train deploy window opens on Thursdays at noon",
            "Deploys moved: changes now ship Monday mornings after the train",
            &[][..],
        ),
        (
            "maximum concurrent rust builders",
            "Ruling: maximum concurrent rust builders is six on the shared host",
            "Ruling, replacing the earlier cap: four builders at once",
            &["ruling"][..],
        ),
    ];
    let mut cases = Vec::new();
    for (query, old_text, new_text, tags) in pairs {
        let old = remember(&state, "project-a", old_text, tags, None).await;
        let new = remember(&state, "project-a", new_text, tags, None).await;
        supersede(&state, "project-a", old, new).await;
        cases.push((query, old, new));
    }

    for surface in [
        Surface::McpRecall,
        Surface::McpRecallDeep,
        Surface::McpRecallAll,
        Surface::ServiceRecall,
        Surface::ServiceRecallAll,
    ] {
        for (query, old, new) in &cases {
            let results = recall_on(&state, surface, "project-a", query).await;
            let (o, n) = (rank_of(&results, *old), rank_of(&results, *new));
            assert!(
                n.is_some(),
                "{surface:?} {query}: replacement recalled: {results:#?}"
            );
            assert!(
                o.is_none() || n < o,
                "{surface:?} {query}: the superseded drawer must rank below its \
                 replacement: {results:#?}"
            );
        }
    }
}

/// Why (#9421 review, HIGH): `memory_recall` and `memory_recall_deep` fold in
/// hits from a user-scope rulings palace. A superseded ruling and its
/// replacement both stored there are joined by an edge in the rulings
/// palace's own KG, which the project palace's lookup never reads.
/// What: per pair, a fresh project palace with one note and a rulings palace
/// holding only that ruling pair, joined by a `superseded_by` edge (one pair
/// per root, so the leg's `ceil(top_k / 3)` cap admits both). In the first
/// pair both drawers answer the query; in the second only the stale one does,
/// so the rulings floor would lift it. On every surface that sees the rulings
/// palace, the replacement is recalled and the old ruling ranks below it.
#[tokio::test]
async fn a_superseded_ruling_from_a_rulings_palace_ranks_below_its_replacement() {
    let pairs = [
        (
            "staging database snapshot retention",
            "Ruling: staging database snapshot retention is seven days",
            "Ruling, replacing the earlier one: staging database snapshot retention is thirty days",
        ),
        (
            "maximum concurrent rust builders",
            "Ruling: maximum concurrent rust builders is six on the shared host",
            "Ruling, replacing the earlier cap: four builders at once",
        ),
    ];
    for (query, old_text, new_text) in pairs {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = state_with(&tmp, &["project-a", "rulings-a"], &["rulings-a"]).await;
        let note = "project note: the billing service pool size is twelve";
        remember(&state, "project-a", note, &[], None).await;
        let old = remember(&state, "rulings-a", old_text, &["ruling"], None).await;
        let new = remember(&state, "rulings-a", new_text, &["ruling"], None).await;
        supersede(&state, "rulings-a", old, new).await;

        for surface in [
            Surface::McpRecall,
            Surface::McpRecallDeep,
            Surface::McpRecallAll,
        ] {
            let results = recall_on(&state, surface, "project-a", query).await;
            let (o, n) = (rank_of(&results, old), rank_of(&results, new));
            assert!(
                n.is_some(),
                "{surface:?} {query}: replacement recalled: {results:#?}"
            );
            assert!(
                o.is_none() || n < o,
                "{surface:?} {query}: the superseded ruling must rank below its \
                 replacement: {results:#?}"
            );
        }
    }
}

/// `(drawer_id, score)` per hit, in rank order.
fn ranked(results: &[Value]) -> Vec<(String, f64)> {
    results
        .iter()
        .map(|r| {
            (
                r["drawer_id"].as_str().expect("drawer_id").to_string(),
                r["score"].as_f64().expect("score"),
            )
        })
        .collect()
}

/// Why (#9421 AC2): a drawer with no `superseded_by` edge keeps its rank.
/// What: recalls a query before and after an edge is written between two
/// other drawers of the same palace. Every hit that is not the superseded
/// drawer keeps its exact score and relative order.
#[tokio::test]
async fn recall_order_is_unchanged_for_drawers_with_no_edge() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with(&tmp, &["project-a"], &[]).await;
    let query = "billing service connection pool";
    for n in 1..=6 {
        remember(
            &state,
            "project-a",
            &format!("{query} note {n}: pool tuning step {n}"),
            &[],
            None,
        )
        .await;
    }
    let old = remember(
        &state,
        "project-a",
        "cron renews gateway certificates",
        &[],
        None,
    )
    .await;
    let new = remember(&state, "project-a", "cert-manager renews certs", &[], None).await;

    let before = ranked(&recall(&state, "project-a", query, 10).await);
    supersede(&state, "project-a", old, new).await;
    let after = ranked(&recall(&state, "project-a", query, 10).await);

    assert!(before.len() >= 6, "{before:#?}");
    let old = old.to_string();
    let others = |v: &[(String, f64)]| -> Vec<(String, f64)> {
        v.iter().filter(|(i, _)| *i != old).cloned().collect()
    };
    assert_eq!(others(&after), others(&before));
}

/// The score of drawer `id` in `results`.
fn score_of(results: &[Value], id: Uuid) -> Option<f64> {
    rank_of(results, id).and_then(|i| results[i]["score"].as_f64())
}

/// Why (#9462, ADR-0028 C9): live drawers carry `superseded_by` edges to
/// drawers that were never written. Such an edge resolves to nothing, so the
/// drawer is the only surviving copy and must keep its full score.
/// What: recalls on every surface before and after an edge from the drawer to
/// a random id with no drawer; the drawer's score is unchanged.
#[tokio::test]
async fn a_drawer_superseded_by_a_missing_drawer_keeps_its_score() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with(&tmp, &["project-a"], &[]).await;
    let query = "gateway certificate renewal schedule";
    let only_copy = remember(
        &state,
        "project-a",
        "gateway certificate renewal schedule: cert-manager renews every sixty days",
        &[],
        None,
    )
    .await;
    remember(&state, "project-a", "billing pool size is twelve", &[], None).await;
    let surfaces = [
        Surface::McpRecall,
        Surface::McpRecallDeep,
        Surface::McpRecallAll,
        Surface::ServiceRecall,
        Surface::ServiceRecallAll,
    ];
    let mut before = Vec::new();
    for surface in surfaces {
        let results = recall_on(&state, surface, "project-a", query).await;
        let score = score_of(&results, only_copy);
        assert!(score.is_some(), "{surface:?}: recalled: {results:#?}");
        before.push(score);
    }

    supersede(&state, "project-a", only_copy, Uuid::new_v4()).await;

    for (surface, before) in surfaces.into_iter().zip(before) {
        let results = recall_on(&state, surface, "project-a", query).await;
        assert_eq!(
            score_of(&results, only_copy),
            before,
            "{surface:?}: a dangling edge must not demote: {results:#?}"
        );
    }
}

/// Opt-in switch for [`recall_supersession_latency_profile`].
const PROFILE_ENV: &str = "TRUSTY_RECALL_SUPERSESSION_PROFILE";
/// Drawers in the profiled project palace (#9421 AC5 states 20k rows).
const PROFILE_DRAWERS: usize = 20_000;
/// Ruling drawers in the profiled rulings palace; every second one replaces
/// the one before it.
const PROFILE_RULINGS: usize = 40;
const PROFILE_QUERIES: usize = 300;

/// `(p50, p95)` of `samples`, in microseconds.
fn percentiles(samples: &mut [std::time::Duration]) -> (u128, u128) {
    samples.sort();
    let at = |q: f64| samples[((samples.len() as f64 * q).ceil() as usize).saturating_sub(1)];
    (at(0.50).as_micros(), at(0.95).as_micros())
}

/// Profile text for drawer or query `n`: one of 97 topics, 89 details.
fn profile_text(n: usize) -> String {
    format!(
        "service topic{} handles detail{} for request path {}",
        n % 97,
        n % 89,
        n
    )
}

/// End-to-end `memory_recall` p50/p95 for `PROFILE_QUERIES` queries.
async fn profile_recall(state: &trusty_memory::AppState) -> (u128, u128) {
    for q in 0..20 {
        recall(state, "project-a", &profile_text(q), 10).await;
    }
    let mut samples = Vec::with_capacity(PROFILE_QUERIES);
    for q in 0..PROFILE_QUERIES {
        let query = profile_text(q * 7_919);
        let start = std::time::Instant::now();
        recall(state, "project-a", &query, 10).await;
        samples.push(start.elapsed());
    }
    percentiles(&mut samples)
}

/// Why (#9421 AC5): the budget is on end-to-end recall p95, before and after
/// the change, at 20k rows; the trusty-common profile times the KG lookup
/// alone. Asserts nothing about timing; run it at both commits and compare.
/// What: a no-op unless `TRUSTY_RECALL_SUPERSESSION_PROFILE` is set. Seeds a
/// 20k-drawer project palace by bulk import (three KG triples per drawer, one
/// drawer in 50 superseded by the next) and a 40-ruling rulings palace (every
/// second ruling replaces the one before),
/// then prints `memory_recall` p50/p95 with the rulings leg off and on. Run:
/// `TRUSTY_RECALL_SUPERSESSION_PROFILE=1 cargo test -p trusty-memory --release
/// --test recall_supersession recall_supersession_latency_profile --
/// --nocapture` (a debug build spends most of an hour seeding the HNSW index).
#[tokio::test(flavor = "multi_thread")]
async fn recall_supersession_latency_profile() {
    use trusty_common::memory_core::palace::{Drawer, PalaceId};
    use trusty_common::memory_core::share::{import_palace_records, SharedMemoryRecord};
    use trusty_common::memory_core::store::kg::Triple;

    if std::env::var_os(PROFILE_ENV).is_none() {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = state_with(&tmp, &["project-a", "rulings-a"], &[]).await;
    let seed_start = std::time::Instant::now();
    let project = state
        .registry
        .open_palace(&state.data_root, &PalaceId::new("project-a"))
        .expect("open project palace");
    // One bulk import: `memory_remember` rebuilds the closet index per write,
    // which is quadratic over 20k drawers.
    let records: Vec<SharedMemoryRecord> = (0..PROFILE_DRAWERS)
        .map(|n| {
            SharedMemoryRecord::from_drawer(
                &Drawer::new(Uuid::new_v4(), profile_text(n)),
                "general",
            )
        })
        .collect();
    import_palace_records(&project, &records)
        .await
        .expect("seed drawers");
    let ids: Vec<Uuid> = {
        let by_text: std::collections::HashMap<String, Uuid> = project
            .drawers
            .read()
            .iter()
            .map(|d| (d.content().to_string(), d.id))
            .collect();
        (0..PROFILE_DRAWERS)
            .map(|n| by_text[&profile_text(n)])
            .collect()
    };
    // Three triples per drawer and one drawer in 50 superseded by the next,
    // the shape `kg_supersession_latency_profile` times in trusty-common.
    let triple = |subject: String, predicate: &str, object: String| Triple {
        subject,
        predicate: predicate.to_string(),
        object,
        valid_from: chrono::Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: None,
    };
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
            rows.push(triple(s, "superseded_by", format!("drawer:{}", ids[n + 1])));
        }
    }
    project
        .kg
        .store()
        .import_all(rows, Vec::new())
        .expect("seed triples");
    let mut old = None;
    for n in 0..PROFILE_RULINGS {
        let text = format!("Ruling: {}", profile_text(n * 13));
        let id = remember(&state, "rulings-a", &text, &["ruling"], None).await;
        match old.take() {
            Some(o) => supersede(&state, "rulings-a", o, id).await,
            None => old = Some(id),
        }
    }
    let seeded = seed_start.elapsed();

    let (p50, p95) = profile_recall(&state).await;
    let with_leg = state.clone().with_rulings_palaces(vec!["rulings-a".into()]);
    let (leg_p50, leg_p95) = profile_recall(&with_leg).await;
    println!(
        "drawers={PROFILE_DRAWERS} rulings={PROFILE_RULINGS} queries={PROFILE_QUERIES} \
         seeded_in={seeded:?} memory_recall p50/p95 us: leg off = {p50}/{p95}, \
         leg on = {leg_p50}/{leg_p95}"
    );
}
