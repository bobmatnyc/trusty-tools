//! `kg-rebuild --merge-punctuated-twins` — fold a pre-#4678 punctuated entity
//! node onto its cleaned twin.
//!
//! Why: #4678 taught the extractor to trim edge punctuation, so a drawer that
//! once yielded `` `redb` `` now yields `redb` and both nodes stand in the same
//! palace. Nothing removes the old one: `is-a`, `uses`, `works-at` and
//! `depends-on` are absent from `kg_store::FUNCTIONAL_PREDICATES`, so a rebuild's
//! assert adds the cleaned spelling beside the punctuated one, and
//! `--purge-stale-subjects` only selects subjects `is_stop_token` rejects —
//! which a real entity never trips. Every rebuild over the same content widens
//! the split (#5401).
//! What: a MERGE, not a delete. Each auto-extracted triple whose subject or
//! object carries edge punctuation is re-asserted under the cleaned identity and
//! only then retracted at its punctuated one, so both positions move and no fact
//! is dropped. Object-position re-pointing is what needed #5396's
//! `retract_triple`: closing the whole `(subject, predicate)` pair would take
//! the punctuated object's correct siblings with it.
//! Test: `twin_repoints_moves_both_positions`, `twin_repoints_skips_namespaces`,
//! `twin_repoints_leaves_the_purge_its_stopwords`,
//! `merge_repoints_both_positions_and_keeps_the_cleaned_nodes_triples`,
//! `merge_reports_a_failed_repoint_and_leaves_the_fact_readable`.

use anyhow::{Context, Result};
use std::collections::HashSet;
use trusty_common::memory_core::store::kg::{KnowledgeGraph, Triple};
use trusty_common::memory_core::store::OpenIntent;

use super::kg_rebuild::{scan_active_triples, STRUCTURAL_PREFIXES};
use crate::kg_extract::{canonical_entity, is_stop_token, AUTO_PROVENANCE};
use crate::AppState;

/// One fact's move from a punctuated node onto the cleaned one.
///
/// Why: the merge has to name both endpoints — what is retracted and what is
/// asserted — because a re-point that only knew the target could not undo the
/// source, and one that only knew the source would delete the fact.
/// What: the stored triple and the same fact under the cleaned identity.
/// `new` differs from `old` in the subject, the object, or both.
/// Test: `twin_repoints_moves_both_positions`.
#[derive(Debug, Clone)]
pub struct TwinRepoint {
    pub old: Triple,
    pub new: Triple,
}

/// Render one re-point for the operator-facing report.
fn render(r: &TwinRepoint) -> String {
    format!(
        "({} {} {}) => ({} {} {})",
        r.old.subject, r.old.predicate, r.old.object, r.new.subject, r.new.predicate, r.new.object
    )
}

/// The `(subject, predicate, object)` identity a triple occupies (#4810).
fn key(t: &Triple) -> (String, String, String) {
    (t.subject.clone(), t.predicate.clone(), t.object.clone())
}

/// The cleaned spelling `term` belongs under, if any.
///
/// Why: `tag:the` and `topic:v1.` are namespaces the user's own tags and rooms
/// live in, not extractor debris — trimming their trailing punctuation would
/// rewrite a membership edge into one pointing at a node nobody wrote.
/// What: [`canonical_entity`] with the [`STRUCTURAL_PREFIXES`] exemption in
/// front of it.
/// Test: `twin_repoints_skips_namespaces`.
fn canonical_term(term: &str) -> Option<&str> {
    if STRUCTURAL_PREFIXES.iter().any(|p| term.starts_with(p)) {
        return None;
    }
    canonical_entity(term)
}

/// Every re-point the active set calls for.
///
/// Why: selection is the half worth testing without a store, and it carries the
/// three guardrails that keep the pass from touching data it does not own — a
/// namespaced term, a triple a human asserted, and a stopword.
/// What: for each active triple stamped [`AUTO_PROVENANCE`], canonicalises the
/// subject and the object independently and emits a [`TwinRepoint`] when either
/// moves. A triple is skipped when the term that would STAY is a stopword:
/// [`canonical_entity`] rejects `("the` in whichever position it sits, so
/// without this check `` `redb` --is-a--> ("the `` would be selected on its
/// subject alone and hang the object garbage off the node users query — and
/// the purge, which selects by subject, could never reach it there. Sorted by
/// the stored triple so the report and the write order are deterministic.
/// Test: `twin_repoints_moves_both_positions`, `twin_repoints_skips_namespaces`,
/// `twin_repoints_spares_a_manual_triple`,
/// `twin_repoints_leaves_the_purge_its_stopwords`.
pub fn twin_repoints(active: &[Triple]) -> Vec<TwinRepoint> {
    let mut out: Vec<TwinRepoint> = Vec::new();
    for t in active {
        // A hand-asserted fact spells its entity the way its author meant to.
        if t.provenance.as_deref() != Some(AUTO_PROVENANCE) {
            continue;
        }
        let subject = canonical_term(&t.subject);
        let object = canonical_term(&t.object);
        if subject.is_none() && object.is_none() {
            continue;
        }
        // #5401: a stopword in EITHER position keeps the triple out of this
        // pass, so the partition with `--purge-stale-subjects` holds for the
        // object position too, not just the subject one.
        if (subject.is_none() && is_stop_token(&t.subject))
            || (object.is_none() && is_stop_token(&t.object))
        {
            continue;
        }
        let mut new = t.clone();
        if let Some(s) = subject {
            new.subject = s.to_string();
        }
        if let Some(o) = object {
            new.object = o.to_string();
        }
        out.push(TwinRepoint {
            old: t.clone(),
            new,
        });
    }
    out.sort_by_key(|a| key(&a.old));
    out
}

/// Per-palace result of a `--merge-punctuated-twins` pass.
///
/// Why: mirrors `PalacePurgeSummary` so one bad palace is reported rather than
/// aborting the run, and so a failed re-point can never be read as a success.
/// What: `selected` is what the scan picked, rendered; `merged` and `failed`
/// split what actually happened. `error` is set whenever anything failed.
/// Test: `merge_summary_counts_each_failure_once`.
#[derive(Debug, Clone)]
pub struct PalaceMergeSummary {
    pub palace_id: String,
    pub selected: Vec<String>,
    pub merged: Vec<String>,
    pub failed: Vec<(String, String)>,
    pub error: Option<String>,
}

impl PalaceMergeSummary {
    /// How many failures this palace contributes to the command's exit code.
    ///
    /// Why: `error` summarises the per-re-point failures when there are any, so
    /// counting both would report every such palace one time too many — the
    /// arithmetic `count_purge_failures` exists to get right on the purge side.
    /// What: the per-re-point failures when there are any, otherwise one for a
    /// palace-level error, otherwise zero. Never both.
    /// Test: `merge_summary_counts_each_failure_once`.
    pub fn failure_count(&self) -> usize {
        if !self.failed.is_empty() {
            self.failed.len()
        } else {
            usize::from(self.error.is_some())
        }
    }
}

/// Print every re-point, then optionally apply it.
///
/// Why: the pass rewrites real facts, so the moves are printed in both modes —
/// an operator sees exactly what moved (or would move) rather than a bare count.
/// What: runs [`merge_palaces`] and prints applied moves on stdout and failures
/// on stderr, then an aggregate. Returns the failure count so the caller can
/// exit non-zero.
/// Test: `merge_dry_run_writes_nothing`.
pub async fn report_merge(
    state: &AppState,
    palace_filter: Option<&str>,
    apply: bool,
) -> Result<usize> {
    let summaries = merge_palaces(state, palace_filter, apply).await?;
    let mut total = 0usize;
    let failures: usize = summaries
        .iter()
        .map(PalaceMergeSummary::failure_count)
        .sum();
    for s in &summaries {
        if let Some(e) = &s.error {
            eprintln!("[merge-error] palace={} error={}", s.palace_id, e);
            if s.failed.is_empty() {
                continue;
            }
        }
        if apply {
            for moved in &s.merged {
                println!("[merge] palace={} repointed {}", s.palace_id, moved);
            }
            for (moved, err) in &s.failed {
                eprintln!(
                    "[merge-FAILED] palace={} repoint={} error={}",
                    s.palace_id, moved, err
                );
            }
            total += s.merged.len();
        } else {
            for moved in &s.selected {
                println!("[merge] palace={} would repoint {}", s.palace_id, moved);
            }
            total += s.selected.len();
        }
    }
    if apply {
        println!("kg-rebuild merge: {total} triples repointed, {failures} failed");
    } else {
        println!("kg-rebuild merge: {total} triples would be repointed (dry run)");
    }
    Ok(failures)
}

/// Select — and optionally apply — punctuated-twin merges across palaces.
///
/// Why: same per-palace error containment as `purge_palaces`. Containment stops
/// at the palace: a registry read that fails yields no palaces to contain, and
/// swallowing it printed `0 triples repointed, 0 failed` over data the pass
/// never read. A TCC denial on the data dir makes that `read_dir` return EPERM,
/// so the clean-looking exit-0 is reachable, not theoretical.
/// What: iterates the registry, filters to one palace when asked, and captures
/// a failing palace as a summary carrying `error` rather than propagating. A
/// failure to list the palaces at all propagates instead.
/// Test: `merge_repoints_both_positions_and_keeps_the_cleaned_nodes_triples`,
/// `merge_dry_run_writes_nothing`,
/// `merge_palaces_propagates_an_unreadable_data_root`.
pub async fn merge_palaces(
    state: &AppState,
    palace_filter: Option<&str>,
    apply: bool,
) -> Result<Vec<PalaceMergeSummary>> {
    let mut out: Vec<PalaceMergeSummary> = Vec::new();
    // #5401: an unreadable data root is a failed run, never an empty one.
    let palaces = trusty_common::memory_core::PalaceRegistry::list_palaces(&state.data_root)
        .with_context(|| format!("list palaces under {}", state.data_root.display()))?;
    for palace in palaces {
        let id = palace.id.0.clone();
        if palace_filter.is_some_and(|filter| filter != id) {
            continue;
        }
        let summary = merge_one(state, &id, apply)
            .await
            .unwrap_or_else(|e| PalaceMergeSummary {
                palace_id: id.clone(),
                selected: Vec::new(),
                merged: Vec::new(),
                failed: Vec::new(),
                error: Some(format!("{e:#}")),
            });
        out.push(summary);
    }
    Ok(out)
}

/// Merge one palace's twins.
///
/// Why: the direct `kg.db` open is the #4678 rule this pass inherits — going
/// through `PalaceHandle::open` would run the issue-#61 expired-drawer sweep,
/// and a preview that deletes drawer rows is not a preview. Intent follows the
/// mode so an applying run fails loud against a running daemon instead of
/// writing into a read-only snapshot.
/// What: scans the active set, selects with [`twin_repoints`], and (when
/// `apply`) re-points each through [`repoint`]. The in-memory adjacency is not
/// resynced — this runs in a one-shot CLI process.
/// Test: `merge_repoints_both_positions_and_keeps_the_cleaned_nodes_triples`,
/// `merge_dry_run_writes_nothing`.
async fn merge_one(state: &AppState, palace_id: &str, apply: bool) -> Result<PalaceMergeSummary> {
    let kg_path = state.data_root.join(palace_id).join("kg.db");
    let intent = if apply {
        OpenIntent::Writer
    } else {
        OpenIntent::ReadOnlyClient
    };
    let kg = KnowledgeGraph::open_with_intent(&kg_path, intent)
        .with_context(|| format!("open kg for palace {palace_id}"))?;

    let active = scan_active_triples(&kg, palace_id).await?;
    let selected = twin_repoints(&active);
    let mut summary = PalaceMergeSummary {
        palace_id: palace_id.to_string(),
        selected: selected.iter().map(render).collect(),
        merged: Vec::new(),
        failed: Vec::new(),
        error: None,
    };
    if !apply {
        return Ok(summary);
    }

    // What the cleaned identity already holds. Re-asserting one of those rows
    // would close its interval and reopen it at the punctuated row's older
    // `valid_from`, leaving a history row whose end precedes its start.
    let mut live: HashSet<(String, String, String)> = active.iter().map(key).collect();
    for r in &selected {
        match repoint(&kg, r, &live).await {
            Ok(()) => {
                live.insert(key(&r.new));
                summary.merged.push(render(r));
            }
            Err(e) => summary.failed.push((render(r), format!("{e:#}"))),
        }
    }
    if !summary.failed.is_empty() {
        summary.error = Some(format!(
            "{} of {} re-point(s) failed",
            summary.failed.len(),
            selected.len()
        ));
    }
    Ok(summary)
}

/// Move one triple onto the cleaned identity.
///
/// Why: assert BEFORE retract. A crash between the two leaves the punctuated
/// twin standing, which the next run merges again; the reverse order loses the
/// fact outright. The retract must be `retract_triple` (#5396) — `retract`
/// closes every object at the pair, so re-pointing `x --uses--> ``redb``` would
/// take `x`'s other `uses` objects with it.
/// What: asserts `new` unless that exact `(subject, predicate, object)` is
/// already live, then closes the punctuated row. Naming a row that is not there
/// is a no-op in both directions, so a re-run is idempotent. A retract that
/// closes nothing is that no-op arriving one step too late — the assert has
/// already landed, so the fact now sits on both nodes, which is the split this
/// pass exists to remove. It becomes a per-re-point failure: the operator sees
/// the pair on `[merge-FAILED]` and the command exits non-zero. An exclusive
/// `OpenIntent::Writer` lock means nothing else can close the row underneath
/// this loop, so it is unreachable today rather than merely unlikely.
/// Test: `merge_repoints_both_positions_and_keeps_the_cleaned_nodes_triples`,
/// `merge_reports_a_failed_repoint_and_leaves_the_fact_readable`.
async fn repoint(
    kg: &KnowledgeGraph,
    r: &TwinRepoint,
    live: &HashSet<(String, String, String)>,
) -> Result<()> {
    // #5401: the cleaned node absorbs the fact before the punctuated one drops it.
    if !live.contains(&key(&r.new)) {
        kg.assert(r.new.clone())
            .await
            .with_context(|| format!("assert cleaned twin {}", render(r)))?;
    }
    let closed = kg
        .retract_triple(&r.old.subject, &r.old.predicate, &r.old.object)
        .await
        .with_context(|| format!("retract punctuated twin {}", render(r)))?;
    anyhow::ensure!(
        closed > 0,
        "punctuated row vanished before its retract, so the fact now stands at both nodes: {}",
        render(r)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::kg_rebuild::stale_subject_candidates;
    use serde_json::json;
    use trusty_common::memory_core::palace::PalaceId;

    /// Build an active triple for the selection tests.
    fn triple(subject: &str, predicate: &str, object: &str, provenance: Option<&str>) -> Triple {
        Triple {
            subject: subject.to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            valid_from: chrono::Utc::now(),
            valid_to: None,
            confidence: 0.6,
            provenance: provenance.map(|p| p.to_string()),
        }
    }

    /// Why: the whole ticket is that an object-position twin is as real as a
    /// subject-position one, and a fix that only re-points subjects leaves half
    /// the split standing.
    /// What: both positions are canonicalised, independently and together.
    /// Test: This test.
    #[test]
    fn twin_repoints_moves_both_positions() {
        let active = vec![
            triple("`redb`", "uses", "mmap", Some(AUTO_PROVENANCE)),
            triple("trusty-memory", "uses", "`redb`", Some(AUTO_PROVENANCE)),
            triple("(sled)", "is-a", "*store*", Some(AUTO_PROVENANCE)),
            triple("redb", "is-a", "database", Some(AUTO_PROVENANCE)),
        ];
        let got = twin_repoints(&active);
        let moved: Vec<(String, String)> = got
            .iter()
            .map(|r| {
                (
                    format!("{} {}", r.old.subject, r.old.object),
                    format!("{} {}", r.new.subject, r.new.object),
                )
            })
            .collect();
        assert_eq!(
            moved,
            vec![
                ("(sled) *store*".to_string(), "sled store".to_string()),
                ("`redb` mmap".to_string(), "redb mmap".to_string()),
                (
                    "trusty-memory `redb`".to_string(),
                    "trusty-memory redb".to_string()
                ),
            ],
            "subject-only, object-only and both-position twins must all move, \
             and an already-clean triple must not"
        );
        assert!(
            got.iter().all(|r| r.old.predicate == r.new.predicate
                && r.old.confidence == r.new.confidence
                && r.old.provenance == r.new.provenance),
            "a re-point changes identity, never the fact's metadata"
        );
    }

    /// Why: `tag:v1.` is a tag the user wrote. Trimming its trailing dot would
    /// re-point a real membership edge onto a node nobody asserted.
    /// What: terms under the drawer/tag/topic/room namespaces never move, in
    /// either position.
    /// Test: This test.
    #[test]
    fn twin_repoints_skips_namespaces() {
        let drawer = format!("drawer:{}", uuid::Uuid::new_v4());
        let active = vec![
            triple("tag:v1.", "tags", &drawer, Some(AUTO_PROVENANCE)),
            triple(
                "topic:(beta)",
                "mentioned-in",
                &drawer,
                Some(AUTO_PROVENANCE),
            ),
            triple("room:*general*", "contains", &drawer, Some(AUTO_PROVENANCE)),
        ];
        assert!(
            twin_repoints(&active).is_empty(),
            "namespaced terms must never be re-pointed"
        );
    }

    /// Why: a human who asserted `` `redb` `` spelled it that way on purpose;
    /// only extractor output is this pass's to rewrite.
    /// What: a punctuated triple stamped anything other than `auto:remember` —
    /// including nothing at all — is not selected.
    /// Test: This test.
    #[test]
    fn twin_repoints_spares_a_manual_triple() {
        let active = vec![
            triple("`redb`", "is-a", "database", Some("kg_assert")),
            triple("`sled`", "is-a", "database", None),
        ];
        assert!(
            twin_repoints(&active).is_empty(),
            "only auto-extracted triples may be re-pointed"
        );
    }

    /// Why: the purge deletes and this merges, so a term both passes claimed
    /// would be deleted and re-pointed in the same run. `is_stop_token` is the
    /// one gate that separates them, and it has to hold in the object position
    /// too: a triple selected on its subject alone once carried `("the` along
    /// as an object and hung it off the cleaned node, where the purge — which
    /// selects by subject — could never reach it again.
    /// What: `("the` is the purge's and not this pass's; `` `redb` `` is this
    /// pass's and not the purge's; and a triple with a stopword at either end
    /// moves neither end.
    /// Test: This test.
    #[test]
    fn twin_repoints_leaves_the_purge_its_stopwords() {
        let active = vec![
            triple("(\"the", "is-a", "thing", Some(AUTO_PROVENANCE)),
            triple("`redb`", "is-a", "database", Some(AUTO_PROVENANCE)),
            // The subject moves, the object is a stopword: selecting this would
            // create `redb --is-a--> ("the`, unreachable garbage on a real node.
            triple("`redb`", "is-a", "(\"the", Some(AUTO_PROVENANCE)),
            // The mirror image — the purge already claims this subject.
            triple("(\"the", "uses", "`redb`", Some(AUTO_PROVENANCE)),
        ];
        let merged: Vec<(String, String)> = twin_repoints(&active)
            .iter()
            .map(|r| (r.new.subject.clone(), r.new.object.clone()))
            .collect();
        assert_eq!(
            merged,
            vec![("redb".to_string(), "database".to_string())],
            "a stopword in either position keeps the whole triple out of this pass"
        );
        assert_eq!(
            stale_subject_candidates(&active),
            vec!["(\"the".to_string()],
            "the two passes must partition the punctuated subjects, not overlap"
        );
    }

    /// Why: the count drives the command's non-zero exit.
    /// What: per-re-point failures count once each; a palace-level error counts
    /// once only when nothing per-re-point failed behind it.
    /// Test: This test.
    #[test]
    fn merge_summary_counts_each_failure_once() {
        let mut s = PalaceMergeSummary {
            palace_id: "a".to_string(),
            selected: vec!["x".to_string(), "y".to_string()],
            merged: vec!["x".to_string()],
            failed: vec![("y".to_string(), "store is read-only".to_string())],
            error: Some("1 of 2 re-point(s) failed".to_string()),
        };
        assert_eq!(s.failure_count(), 1, "the error merely summarises the one");
        s.failed.clear();
        assert_eq!(s.failure_count(), 1, "a palace-level error is one failure");
        s.error = None;
        assert_eq!(s.failure_count(), 0);
    }

    /// Seed a palace in a tempdir and return its `AppState`.
    async fn palace_fixture(data_root: std::path::PathBuf) -> Result<AppState> {
        trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();
        // Issue #88: bypass palace-slug enforcement for test palaces.
        // SAFETY: idempotent constant write "1"; safe across test threads.
        unsafe {
            std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
        }
        let state = AppState::new(data_root);
        state.set_ready();
        let _ = crate::tools::dispatch_tool(&state, "palace_create", json!({"name": "a"})).await?;
        Ok(state)
    }

    /// Every active object at `subject`, sorted.
    async fn objects_of(kg: &KnowledgeGraph, subject: &str) -> Result<Vec<String>> {
        let mut got: Vec<String> = kg
            .query_active(subject)
            .await?
            .into_iter()
            .map(|t| t.object)
            .collect();
        got.sort();
        Ok(got)
    }

    /// Why: the closure bar #5401 sets. A pass that deletes the punctuated node
    /// satisfies "there is only one node" while silently dropping every fact
    /// that node participated in, and one that re-points only the subject side
    /// leaves the object-position twin behind. This asserts the merged node
    /// holds BOTH its own pre-existing triples and the re-pointed ones, on both
    /// sides of the edge.
    /// What: seeds `redb --is-a--> database` (already clean), `` `redb`
    /// --uses--> mmap `` (subject-position twin) and `trusty-memory --uses-->
    /// ``redb``` (object-position twin) plus a manual `` `sled` `` triple, then
    /// applies the merge and reads every position back.
    /// Test: This test.
    #[tokio::test]
    async fn merge_repoints_both_positions_and_keeps_the_cleaned_nodes_triples() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let state = palace_fixture(tmp.path().to_path_buf()).await?;
        let handle = state
            .registry
            .open_palace(&state.data_root, &PalaceId::new("a"))?;
        for t in [
            triple("redb", "is-a", "database", Some(AUTO_PROVENANCE)),
            triple("`redb`", "uses", "mmap", Some(AUTO_PROVENANCE)),
            triple("trusty-memory", "uses", "`redb`", Some(AUTO_PROVENANCE)),
            triple("trusty-memory", "uses", "tokio", Some(AUTO_PROVENANCE)),
            triple("`sled`", "is-a", "store", Some("kg_assert")),
        ] {
            handle.kg.assert(t).await?;
        }

        let applied = merge_palaces(&state, Some("a"), true).await?;
        assert_eq!(applied.len(), 1);
        assert!(
            applied[0].failed.is_empty(),
            "no re-point should have failed"
        );
        assert!(applied[0].error.is_none());
        assert_eq!(applied[0].merged.len(), 2, "two twins must have moved");

        assert_eq!(
            objects_of(&handle.kg, "redb").await?,
            vec!["database".to_string(), "mmap".to_string()],
            "the merged node must keep its OWN pre-existing triple and gain the re-pointed one"
        );
        assert!(
            handle.kg.query_active("`redb`").await?.is_empty(),
            "the punctuated node must be gone from the subject position"
        );
        assert_eq!(
            objects_of(&handle.kg, "trusty-memory").await?,
            vec!["redb".to_string(), "tokio".to_string()],
            "the object position must move onto the cleaned node without \
             taking its sibling objects at the same predicate down"
        );
        assert_eq!(
            objects_of(&handle.kg, "`sled`").await?,
            vec!["store".to_string()],
            "a manually asserted punctuated triple must be untouched"
        );

        // Re-running must be a no-op rather than a second round of writes.
        let again = merge_palaces(&state, Some("a"), true).await?;
        assert!(
            again[0].selected.is_empty(),
            "the merge must be idempotent, got {:?}",
            again[0].selected
        );
        Ok(())
    }

    /// Why: the preview an operator reaches for first must not be the thing
    /// that rewrites their graph.
    /// What: a report-only run lists the twin and leaves both nodes in place.
    /// Test: This test.
    #[tokio::test]
    async fn merge_dry_run_writes_nothing() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let state = palace_fixture(tmp.path().to_path_buf()).await?;
        let handle = state
            .registry
            .open_palace(&state.data_root, &PalaceId::new("a"))?;
        handle
            .kg
            .assert(triple("`redb`", "uses", "mmap", Some(AUTO_PROVENANCE)))
            .await?;

        let dry = merge_palaces(&state, Some("a"), false).await?;
        assert_eq!(dry[0].selected.len(), 1, "the twin must be reported");
        assert!(dry[0].merged.is_empty(), "a dry run repoints nothing");
        assert!(
            !handle.kg.query_active("`redb`").await?.is_empty(),
            "a dry run must leave the punctuated node standing"
        );
        assert!(
            handle.kg.query_active("redb").await?.is_empty(),
            "a dry run must not create the cleaned node either"
        );
        assert_eq!(report_merge(&state, Some("a"), false).await?, 0);
        Ok(())
    }

    /// Why: `list_palaces` was called with `unwrap_or_default()`, so a registry
    /// read that failed became zero palaces and the run printed
    /// `0 triples repointed, 0 failed` and exited 0 over data it never read. A
    /// macOS TCC denial on the data dir is exactly that failure — `read_dir`
    /// returns EPERM — and the operator's evidence that the merge ran clean
    /// would be a pass that never opened a single palace.
    /// What: a `data_root` whose listing fails makes the pass return `Err`.
    /// A regular file stands in for the denial: both reach the same `read_dir`
    /// error arm, and this one reproduces without depending on the uid the
    /// suite runs as.
    /// Test: This test.
    #[tokio::test]
    async fn merge_palaces_propagates_an_unreadable_data_root() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let data_root = tmp.path().join("not-a-directory");
        std::fs::write(&data_root, b"")?;
        let state = AppState::new(data_root);

        let err = merge_palaces(&state, None, false)
            .await
            .expect_err("an unlistable data root must fail the run, not report an empty one");
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("list palaces"),
            "the error must name what could not be read, got {rendered}"
        );
        Ok(())
    }

    /// Why: assert-before-retract is this pass's whole crash-safety argument,
    /// and until this test nothing ran a re-point that failed — the ordering
    /// was verified only by reading the source. A failed write must leave the
    /// fact readable SOMEWHERE, be reported as failed rather than merged, and
    /// reach the non-zero exit at `kg_rebuild.rs`.
    /// What: seeds `` `redb` --uses--> mmap ``, then hands the merge a store
    /// whose writes are rejected — the live redb file is held open, so the
    /// process-wide store cache is primed with a read-only snapshot and the
    /// merge's `Writer` open picks it up. Asserts the failure is recorded, that
    /// `report_merge` returns a non-zero count, and that the fact is still at
    /// the punctuated node afterwards.
    /// Test: This test.
    #[tokio::test]
    async fn merge_reports_a_failed_repoint_and_leaves_the_fact_readable() -> Result<()> {
        let seed_root = tempfile::tempdir()?;
        let seeded = palace_fixture(seed_root.path().to_path_buf()).await?;
        let handle = seeded
            .registry
            .open_palace(&seeded.data_root, &PalaceId::new("a"))?;
        handle
            .kg
            .assert(triple("`redb`", "uses", "mmap", Some(AUTO_PROVENANCE)))
            .await?;

        // Copy the committed palace onto a second data root the registry has
        // never opened, so this test owns every handle on that redb file.
        let tmp = tempfile::tempdir()?;
        let palace_dir = tmp.path().join("a");
        std::fs::create_dir_all(&palace_dir)?;
        for entry in std::fs::read_dir(seeded.data_root.join("a"))? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                std::fs::copy(entry.path(), palace_dir.join(entry.file_name()))?;
            }
        }
        let kg_path = palace_dir.join("kg.db");
        let state = AppState::new(tmp.path().to_path_buf());
        state.set_ready();

        // Holding the live file makes the next open fall back to a read-only
        // snapshot, which the store cache then hands to the merge.
        let live = redb::Database::create(palace_dir.join("kg.redb"))
            .context("hold the palace's redb lock")?;
        let read_only = KnowledgeGraph::open_with_intent(&kg_path, OpenIntent::ReadOnlyClient)?;

        let applied = merge_palaces(&state, Some("a"), true).await?;
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].selected.len(), 1, "the twin must still be found");
        assert!(
            applied[0].merged.is_empty(),
            "a re-point that could not write must never be reported as merged"
        );
        assert_eq!(applied[0].failed.len(), 1, "the failure must be recorded");
        assert!(
            applied[0].failed[0].1.contains("read-only"),
            "the failure must carry its error text, got {:?}",
            applied[0].failed[0].1
        );
        assert!(
            applied[0].error.is_some(),
            "a palace with a failed re-point must carry an error"
        );
        assert!(
            report_merge(&state, Some("a"), true).await? > 0,
            "the failure must reach the count kg_rebuild bails on"
        );

        // Assert-before-retract: the fact is still where it was.
        drop(read_only);
        drop(live);
        let after = KnowledgeGraph::open_with_intent(&kg_path, OpenIntent::Writer)?;
        assert_eq!(
            objects_of(&after, "`redb`").await?,
            vec!["mmap".to_string()],
            "a re-point that failed must leave the fact readable at the punctuated node"
        );
        Ok(())
    }
}
