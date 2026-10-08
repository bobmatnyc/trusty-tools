//! Unit tests for `tools::recall_supersede` (#9421).

use super::*;
use crate::service::core_tests::LogCapture;
use crate::tools::recall_rank::{by_score_desc, demote_stale_snapshots};
use chrono::Utc;
use trusty_common::memory_core::palace::Drawer;

fn hit(score: f32) -> RecallResult {
    RecallResult {
        drawer: Drawer::new(Uuid::new_v4(), "d"),
        score,
        layer: 2,
    }
}

fn id(r: &RecallResult) -> Uuid {
    r.drawer.id
}

/// Sort the way every recall path does after demotion.
fn ranked(mut results: Vec<RecallResult>, sup: &Supersessions) -> Vec<RecallResult> {
    demote_superseded(&mut results, sup);
    results.sort_by(by_score_desc);
    results
}

fn rank(results: &[RecallResult], drawer: Uuid) -> usize {
    results
        .iter()
        .position(|r| r.drawer.id == drawer)
        .expect("drawer present")
}

/// Why (#9421 AC1): the old drawer out-scores its replacement by far more
/// than the weight, and must still rank below it.
#[test]
fn a_superseded_drawer_ranks_below_a_replacement_it_outscored() {
    let (old, new, other) = (hit(0.95), hit(0.20), hit(0.40));
    let sup = Supersessions::from([(id(&old), id(&new))]);
    let (o, n) = (id(&old), id(&new));
    let out = ranked(vec![old, new, other], &sup);
    assert!(rank(&out, n) < rank(&out, o), "{out:#?}");
    assert!(out[rank(&out, o)].score < out[rank(&out, n)].score);
}

/// Why: demotion, not exclusion (ADR-0028 D6) — with no replacement in the
/// window the old drawer keeps half its score and stays recalled.
#[test]
fn a_superseded_drawer_is_halved_when_its_replacement_is_absent() {
    let old = hit(0.8);
    let sup = Supersessions::from([(id(&old), Uuid::new_v4())]);
    let out = ranked(vec![old], &sup);
    assert_eq!(out.len(), 1);
    assert!((out[0].score - 0.8 * SUPERSEDED_WEIGHT).abs() < 1e-6);
}

/// Why: A superseded by B, B superseded by C — every link holds at once.
#[test]
fn a_supersession_chain_ranks_in_order() {
    let (a, b, c) = (hit(0.9), hit(0.8), hit(0.1));
    let (ia, ib, ic) = (id(&a), id(&b), id(&c));
    let sup = Supersessions::from([(ia, ib), (ib, ic)]);
    let out = ranked(vec![a, b, c], &sup);
    assert!(rank(&out, ic) < rank(&out, ib), "{out:#?}");
    assert!(rank(&out, ib) < rank(&out, ia), "{out:#?}");
}

/// Why (#9421 AC2): a drawer with no edge keeps its rank. What: around one
/// superseded pair, every other drawer keeps its exact score and relative
/// order; with no edges at all the list is untouched.
#[test]
fn drawers_with_no_edge_keep_their_scores_and_order() {
    let scores = [0.91, 0.85, 0.77, 0.64, 0.52, 0.33, 0.21];
    let results: Vec<RecallResult> = scores.iter().map(|s| hit(*s)).collect();
    let before: Vec<(Uuid, f32)> = results.iter().map(|r| (id(r), r.score)).collect();

    let untouched = ranked(results.clone(), &Supersessions::new());
    let after: Vec<(Uuid, f32)> = untouched.iter().map(|r| (id(r), r.score)).collect();
    assert_eq!(after, before);

    let (old, new) = (before[0].0, before[4].0);
    let out = ranked(results, &Supersessions::from([(old, new)]));
    let others = |v: &[(Uuid, f32)]| -> Vec<(Uuid, f32)> {
        v.iter().copied().filter(|(i, _)| *i != old).collect()
    };
    let after: Vec<(Uuid, f32)> = out.iter().map(|r| (id(r), r.score)).collect();
    assert_eq!(others(&after), others(&before), "{out:#?}");
}

/// Why: the snapshot weight runs first; the replacement may itself be a stale
/// snapshot and lose half its score, and the order must survive both weights.
#[test]
fn a_superseded_snapshot_stays_below_its_replacement_after_both_weights() {
    let now = Utc::now();
    let mut new = hit(0.6);
    new.drawer.tags = vec!["status".into()];
    new.drawer.created_at = now - chrono::Duration::days(30);
    let old = hit(0.5);
    let (o, n) = (id(&old), id(&new));
    let mut results = vec![old, new];
    demote_stale_snapshots(&mut results, now, &Supersessions::from([(o, n)]));
    assert!(rank(&results, n) < rank(&results, o), "{results:#?}");
}

/// Why: a NaN score sorts first; a superseded NaN must not stay there.
#[test]
fn a_superseded_nan_score_ranks_below_its_replacement() {
    let (old, new) = (hit(f32::NAN), hit(0.3));
    let (o, n) = (id(&old), id(&new));
    let out = ranked(vec![old, new], &Supersessions::from([(o, n)]));
    assert!(rank(&out, n) < rank(&out, o), "{out:#?}");
}

/// Why (#9421 AC3): a KG read error must not drop or error the recall; it
/// demotes nothing and says so in the log.
#[tokio::test]
async fn a_failed_lookup_demotes_nothing() {
    let err = lookup_within(
        async { Err(anyhow::anyhow!("kg.redb unreadable")) },
        LOOKUP_BUDGET,
    )
    .await
    .expect_err("a read error is an error here");
    assert!(matches!(err, SupersessionLookupError::Read(_)), "{err:?}");

    let (log, _guard) = LogCapture::install();
    let found = fail_open(
        "p-9421",
        async { Err(anyhow::anyhow!("kg.redb unreadable")) },
        LOOKUP_BUDGET,
    )
    .await;
    assert!(found.is_empty());
    let lines = log.lines_naming("#9421");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("WARN") && lines[0].contains("kg.redb unreadable"));
    assert!(lines[0].contains("p-9421"), "{lines:?}");
}

/// Why (#9421 AC3): a stalled KG read must not hold the recall past its
/// budget; it demotes nothing and says so in the log.
#[tokio::test]
async fn a_late_lookup_demotes_nothing() {
    // A short budget keeps the test fast; `LOOKUP_BUDGET` is the same code path.
    let budget = Duration::from_millis(20);
    let stalled = std::future::pending::<anyhow::Result<Supersessions>>();
    let err = lookup_within(stalled, budget)
        .await
        .expect_err("a stalled read times out");
    assert!(
        matches!(err, SupersessionLookupError::TimedOut(_)),
        "{err:?}"
    );

    let (log, _guard) = LogCapture::install();
    let stalled = std::future::pending::<anyhow::Result<Supersessions>>();
    let found = fail_open("p-9421", stalled, budget).await;
    assert!(found.is_empty());
    let lines = log.lines_naming("#9421");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("budget"), "{lines:?}");
}
