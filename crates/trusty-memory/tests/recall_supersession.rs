//! End-to-end supersession demotion (#9421): a drawer superseded through a
//! `superseded_by` edge, with no snapshot tag and no `fact_key`, ranks below
//! the drawer that replaced it on every recall surface.
//!
//! Why: `tools::recall_supersede_tests` proves the ranking rule; these prove
//! each recall surface reads the edge from the palace KG and applies it.
//! What: drawers written through `memory_remember`, the edge written through
//! trusty-common's `assert_superseded_by` (the dream and share writer), then
//! recalled. Its own binary, so seeding the mock embedder cannot race the
//! lib tests' real singleton. The fail-open arm (KG read error or timeout) is
//! unit-tested in `tools::recall_supersede_tests`.
//! Test: this IS the test module.

mod recall_support;

use recall_support::{rank_of, recall, recall_on, remember, state_with, supersede, Surface};
use serde_json::Value;

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
