//! `trusty-memory kg-rebuild` — back-fill auto-extracted KG triples.
//!
//! Why: Issue #97 — `memory_remember` and `memory_note` now run a
//! deterministic KG extraction pass on every write, but palaces that were
//! populated before this feature shipped sit at zero auto-extracted triples.
//! The `kg-rebuild` command re-runs extraction across every drawer in a
//! palace (or every palace, when `--palace` is omitted) so the visual graph
//! view is immediately useful.
//! What: A blocking-friendly handler that opens each palace via the standard
//! `AppState` flow, walks the palace's drawer table, runs `extract_triples`,
//! and asserts every result through `KnowledgeGraph::assert`. Errors are
//! aggregated per palace; one bad palace never aborts the rest of the run.
//! Test: `kg_rebuild_processes_all_drawers`,
//! `kg_rebuild_processes_named_palace_only`.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use trusty_common::memory_core::palace::PalaceId;
use trusty_common::memory_core::store::kg::Triple;

use crate::kg_extract::{
    extract_triples, is_stop_token, ExtractInput, AUTO_PROVENANCE, DRAWER_SUBJECT_PREFIX,
    ROOM_SUBJECT_PREFIX, TAG_SUBJECT_PREFIX, TOPIC_SUBJECT_PREFIX,
};
use crate::{resolve_palace_registry_dir, AppState};

/// Subject namespaces the extractor owns and the purge must never touch.
///
/// Why: #4678's purge targets the bare tokens the pattern pass invents. A
/// `tag:the` subject is a tag the user actually wrote, not extraction debris,
/// and deleting it would destroy real membership edges.
/// What: the four prefixes `kg_extract` emits. A subject carrying any of them
/// is skipped before the stopword test runs.
/// Test: `purge_skips_namespaced_subjects`.
const STRUCTURAL_PREFIXES: &[&str] = &[
    DRAWER_SUBJECT_PREFIX,
    TAG_SUBJECT_PREFIX,
    TOPIC_SUBJECT_PREFIX,
    ROOM_SUBJECT_PREFIX,
];

/// How many active triples to pull per `list_active` page during a purge scan.
const PURGE_SCAN_PAGE: usize = 1000;

/// Summary returned to the CLI per palace.
///
/// Why: Operators need a per-palace count of drawers scanned and triples
/// asserted so they can confirm the back-fill actually wrote something.
/// What: Carries the palace id, drawers scanned, triples asserted, and any
/// per-palace error captured as a string (so a failure on one palace can be
/// logged without aborting the rest of the run).
/// Test: `kg_rebuild_processes_all_drawers` asserts the field values.
#[derive(Debug, Clone)]
pub struct PalaceRebuildSummary {
    pub palace_id: String,
    pub drawers_scanned: usize,
    pub triples_asserted: usize,
    pub error: Option<String>,
}

/// What a `kg-rebuild` invocation was asked to do.
///
/// Why: #4678 added two flags to a command that had one. Bundling them keeps
/// [`handle_kg_rebuild`]'s existing one-argument signature intact — that
/// function is public API of a crate `trusty-agents` links against, so
/// widening it in place would be a breaking change for a bug fix.
/// What: the palace filter plus the two purge switches. `Default` is an
/// ordinary rebuild of every palace, which is the pre-#4678 behaviour.
/// Test: `purge_is_off_by_default`.
#[derive(Debug, Clone, Default)]
pub struct KgRebuildOptions {
    /// Restrict the run to a single palace id. `None` processes every palace.
    pub palace: Option<String>,
    /// Delete auto-extracted subjects the #4678 token filter now rejects.
    pub purge_stale_subjects: bool,
    /// Report the purge candidates and write nothing at all.
    pub dry_run: bool,
}

/// CLI entry point for `trusty-memory kg-rebuild`.
///
/// Why: preserved unchanged as the crate's public one-argument entry point
/// (see [`KgRebuildOptions`]).
/// What: delegates to [`handle_kg_rebuild_with`] with both purge switches off,
/// which is exactly the pre-#4678 behaviour.
/// Test: covered through `handle_kg_rebuild_with`.
pub async fn handle_kg_rebuild(palace: Option<String>) -> Result<()> {
    handle_kg_rebuild_with(KgRebuildOptions {
        palace,
        ..Default::default()
    })
    .await
}

/// Run `kg-rebuild` with the full #4678 option set.
///
/// Why: A thin shim that resolves the standard data dir, builds an
/// `AppState`, loads every palace, and dispatches to `rebuild_palaces`. Kept
/// separate from the core logic so the test suite can exercise
/// `rebuild_palaces` against a temp directory without going through clap.
/// What: Resolves `~/Library/Application Support/trusty-memory` (or the
/// platform equivalent) via `resolve_data_dir`, calls `rebuild_palaces` with
/// the optional palace filter, then prints a human-readable summary to
/// stdout. With `purge_stale_subjects` it then deletes the stale subjects;
/// with `dry_run` it skips the rebuild entirely and only reports what the
/// purge would delete, so the whole invocation writes nothing.
/// Test: not unit-tested (process-level entry point); `rebuild_palaces` and
/// `purge_palaces` are the testable surfaces.
pub async fn handle_kg_rebuild_with(opts: KgRebuildOptions) -> Result<()> {
    let data_dir = trusty_common::resolve_data_dir("trusty-memory")
        .context("resolve trusty-memory data dir")?;
    let data_root = resolve_palace_registry_dir(data_dir);
    let state = AppState::new(data_root);
    let loaded = state
        .load_palaces_from_disk()
        .await
        .context("load palaces from disk")?;
    tracing::info!(palaces_loaded = loaded, "kg-rebuild: palaces opened");

    let palace = opts.palace.clone();
    if opts.dry_run {
        // Report-only: no re-assert pass, no deletion. Nothing is written.
        println!("kg-rebuild: DRY RUN — no triples will be asserted or deleted");
        report_purge(&state, palace.as_deref(), false).await?;
        return Ok(());
    }

    let summaries = rebuild_palaces(&state, palace.as_deref()).await?;
    let mut total_drawers = 0usize;
    let mut total_triples = 0usize;
    let mut total_errors = 0usize;
    for s in &summaries {
        if let Some(e) = &s.error {
            total_errors += 1;
            eprintln!(
                "[error] palace={} drawers={} triples={} error={}",
                s.palace_id, s.drawers_scanned, s.triples_asserted, e
            );
        } else {
            println!(
                "[ok]    palace={} drawers={} triples={}",
                s.palace_id, s.drawers_scanned, s.triples_asserted
            );
        }
        total_drawers += s.drawers_scanned;
        total_triples += s.triples_asserted;
    }
    println!(
        "kg-rebuild complete: {} palaces processed, {} drawers scanned, {} triples asserted, {} errors",
        summaries.len(),
        total_drawers,
        total_triples,
        total_errors
    );
    if opts.purge_stale_subjects {
        report_purge(&state, palace.as_deref(), true).await?;
    }
    Ok(())
}

/// Print every stale subject, then optionally delete it.
///
/// Why: the purge is destructive against real data, so the subject list is
/// printed in both modes — an operator sees exactly what was (or would be)
/// removed rather than a bare count.
/// What: runs [`purge_palaces`] and prints one line per subject plus an
/// aggregate. `apply` selects deletion versus report-only.
/// Test: covered through `purge_palaces`.
async fn report_purge(state: &AppState, palace_filter: Option<&str>, apply: bool) -> Result<()> {
    let summaries = purge_palaces(state, palace_filter, apply).await?;
    let verb = if apply { "deleted" } else { "would delete" };
    let mut total_subjects = 0usize;
    let mut total_rows = 0usize;
    for s in &summaries {
        if let Some(e) = &s.error {
            eprintln!("[error] purge palace={} error={}", s.palace_id, e);
            continue;
        }
        for subject in &s.subjects {
            println!(
                "[purge] palace={} {} subject={}",
                s.palace_id, verb, subject
            );
        }
        total_subjects += s.subjects.len();
        total_rows += s.rows_closed;
    }
    if apply {
        println!("kg-rebuild purge: {total_subjects} subjects deleted, {total_rows} rows closed");
    } else {
        println!("kg-rebuild purge: {total_subjects} subjects would be deleted (dry run)");
    }
    Ok(())
}

/// Per-palace result of a `--purge-stale-subjects` pass.
///
/// Why: mirrors [`PalaceRebuildSummary`] so one bad palace is reported without
/// aborting the rest of the run.
/// What: the subjects selected, how many triple rows were closed (always 0 in
/// report-only mode), and any per-palace error.
/// Test: `purge_selects_only_stopword_subjects`.
#[derive(Debug, Clone)]
pub struct PalacePurgeSummary {
    pub palace_id: String,
    pub subjects: Vec<String>,
    pub rows_closed: usize,
    pub error: Option<String>,
}

/// Select — and optionally delete — stale auto-extracted subjects.
///
/// Why: #4678's forward filter only stops NEW garbage. `rebuild_one` re-asserts
/// and never retracts, and the four pattern predicates are absent from
/// `FUNCTIONAL_PREDICATES`, so `assert` supersedes only an identical object.
/// Without this pass every triple already in the graph stays there forever.
/// What: per palace, scans every active triple, picks the subjects
/// [`stale_subject_candidates`] returns, and (when `apply`) calls the existing
/// `delete_by_subject` on each. Errors are captured per palace.
/// Test: `purge_selects_only_stopword_subjects`, `purge_skips_namespaced_subjects`.
pub async fn purge_palaces(
    state: &AppState,
    palace_filter: Option<&str>,
    apply: bool,
) -> Result<Vec<PalacePurgeSummary>> {
    let mut out: Vec<PalacePurgeSummary> = Vec::new();
    let palaces = trusty_common::memory_core::PalaceRegistry::list_palaces(&state.data_root)
        .unwrap_or_default();
    for palace in palaces {
        let id = palace.id.0.clone();
        if let Some(filter) = palace_filter {
            if filter != id {
                continue;
            }
        }
        let summary = purge_one(state, &id, apply)
            .await
            .unwrap_or_else(|e| PalacePurgeSummary {
                palace_id: id.clone(),
                subjects: Vec::new(),
                rows_closed: 0,
                error: Some(format!("{e:#}")),
            });
        out.push(summary);
    }
    Ok(out)
}

/// Purge a single palace.
///
/// Why: keeps the per-palace work in one focused function, matching
/// `rebuild_one`.
/// What: pages `list_active` into one vec, runs [`stale_subject_candidates`],
/// then deletes each match through `KgStoreRedb::delete_by_subject` on the
/// blocking pool. The in-memory adjacency is not resynced — this runs in a
/// one-shot CLI process that exits immediately afterwards.
/// Test: `purge_selects_only_stopword_subjects`.
async fn purge_one(state: &AppState, palace_id: &str, apply: bool) -> Result<PalacePurgeSummary> {
    let pid = PalaceId::new(palace_id);
    let handle = state
        .registry
        .open_palace(&state.data_root, &pid)
        .with_context(|| format!("open palace {palace_id}"))?;

    let mut active: Vec<Triple> = Vec::new();
    let mut offset = 0usize;
    loop {
        let page = handle
            .kg
            .list_active(PURGE_SCAN_PAGE, offset)
            .await
            .with_context(|| format!("scan active triples in {palace_id}"))?;
        let n = page.len();
        active.extend(page);
        if n < PURGE_SCAN_PAGE {
            break;
        }
        offset += n;
    }

    let subjects = stale_subject_candidates(&active);
    let mut rows_closed = 0usize;
    if apply {
        for subject in &subjects {
            let store = handle.kg.store();
            let s = subject.clone();
            match tokio::task::spawn_blocking(move || store.delete_by_subject(&s)).await {
                Ok(Ok(n)) => rows_closed += n,
                Ok(Err(e)) => tracing::warn!(
                    palace = %palace_id,
                    subject = %subject,
                    "kg-rebuild purge: delete failed (non-fatal): {e:#}",
                ),
                Err(e) => tracing::warn!(
                    palace = %palace_id,
                    subject = %subject,
                    "kg-rebuild purge: delete task join error: {e:#}",
                ),
            }
        }
    }
    Ok(PalacePurgeSummary {
        palace_id: palace_id.to_string(),
        subjects,
        rows_closed,
        error: None,
    })
}

/// Pick the subjects a purge may delete.
///
/// Why: `delete_by_subject` removes EVERY active triple under a subject, so
/// selection has to be conservative on two axes at once — the subject must be
/// one the extractor invented (not a user's tag or room, not a hand-asserted
/// fact), and it must be one the #4678 filter would now reject.
/// What: a subject qualifies only when all three hold: it carries none of
/// [`STRUCTURAL_PREFIXES`]; every one of its active triples is stamped
/// `auto:remember`, so a single manual assert protects it; and
/// `is_stop_token` rejects it. Returns distinct subjects, sorted.
/// Test: `purge_selects_only_stopword_subjects`, `purge_skips_namespaced_subjects`,
/// `purge_spares_a_subject_with_a_manual_triple`.
pub fn stale_subject_candidates(active: &[Triple]) -> Vec<String> {
    let mut by_subject: BTreeMap<&str, bool> = BTreeMap::new();
    for t in active {
        if STRUCTURAL_PREFIXES.iter().any(|p| t.subject.starts_with(p)) {
            continue;
        }
        if !is_stop_token(&t.subject) {
            continue;
        }
        let all_auto = by_subject.entry(t.subject.as_str()).or_insert(true);
        *all_auto &= t.provenance.as_deref() == Some(AUTO_PROVENANCE);
    }
    by_subject
        .into_iter()
        .filter(|(_, all_auto)| *all_auto)
        .map(|(s, _)| s.to_string())
        .collect()
}

/// Run KG back-fill across one or every palace in an `AppState`.
///
/// Why: Pulled out as a testable async function so the unit tests can build
/// an `AppState` rooted at a tempdir, populate a palace with drawers via the
/// real `memory_remember` path, drop the auto-extracted triples on the floor
/// (by retracting), and then re-run `rebuild_palaces` to confirm it can
/// reseed the KG end-to-end without touching the CLI surface.
/// What: When `palace_filter` is `Some`, processes only the matching palace;
/// otherwise iterates every loaded palace via `PalaceRegistry::list_palaces`.
/// Each palace is processed inside its own `rebuild_one` call so a single
/// failure is captured per-palace rather than aborting the run.
/// Test: `kg_rebuild_processes_all_drawers`,
/// `kg_rebuild_processes_named_palace_only`.
pub async fn rebuild_palaces(
    state: &AppState,
    palace_filter: Option<&str>,
) -> Result<Vec<PalaceRebuildSummary>> {
    let mut out: Vec<PalaceRebuildSummary> = Vec::new();
    let palaces = trusty_common::memory_core::PalaceRegistry::list_palaces(&state.data_root)
        .unwrap_or_default();
    for palace in palaces {
        let id = palace.id.0.clone();
        if let Some(filter) = palace_filter {
            if filter != id {
                continue;
            }
        }
        let summary = rebuild_one(state, &id)
            .await
            .unwrap_or_else(|e| PalaceRebuildSummary {
                palace_id: id.clone(),
                drawers_scanned: 0,
                triples_asserted: 0,
                error: Some(format!("{e:#}")),
            });
        out.push(summary);
    }
    Ok(out)
}

/// Back-fill a single palace.
///
/// Why: Keeps the per-palace work in one focused function so error capture
/// stays clean and the iteration over drawers reads top-to-bottom.
/// What: Opens the palace handle, snapshots the drawer table, runs
/// `extract_triples` on each drawer, and calls `handle.kg.assert` for every
/// result. Failures on individual `assert` calls are logged but don't abort
/// the rest of the drawers — the function only returns `Err` on hard failure
/// to open the palace or read the drawer list.
/// Test: `kg_rebuild_processes_all_drawers` (drawer count and asserted count
/// must match the heuristic expectations).
async fn rebuild_one(state: &AppState, palace_id: &str) -> Result<PalaceRebuildSummary> {
    let pid = PalaceId::new(palace_id);
    let handle = state
        .registry
        .open_palace(&state.data_root, &pid)
        .with_context(|| format!("open palace {palace_id}"))?;

    let drawers = handle.drawers.read().clone();
    let mut asserted = 0usize;
    for d in &drawers {
        let room = room_id_to_label(d.room_id);
        let triples = extract_triples(&ExtractInput {
            drawer_id: d.id,
            content: &d.content,
            tags: &d.tags,
            room: room.as_deref(),
        });
        for triple in triples {
            let s = triple.subject.clone();
            let p = triple.predicate.clone();
            match handle.kg.assert(triple).await {
                Ok(()) => asserted += 1,
                Err(e) => tracing::warn!(
                    palace = %palace_id,
                    drawer_id = %d.id,
                    subject = %s,
                    predicate = %p,
                    "kg-rebuild: assert failed (non-fatal): {e:#}",
                ),
            }
        }
    }
    Ok(PalaceRebuildSummary {
        palace_id: palace_id.to_string(),
        drawers_scanned: drawers.len(),
        triples_asserted: asserted,
        error: None,
    })
}

/// Recover a friendly room label from a drawer's `room_id` UUID.
///
/// Why: `Drawer` only stores the hashed `room_id`, but the auto-extractor
/// wants a human-readable label so the back-filled graph matches what fresh
/// writes produce. Re-deriving the label from the deterministic hash is
/// brittle (the hashing function isn't a public API); for the back-fill case
/// we accept that room labels are absent and let the rest of the extraction
/// proceed.
/// What: Currently returns `None` unconditionally. Future versions can wire
/// in the reverse mapping when `room_to_uuid` becomes public.
/// Test: indirect via `kg_rebuild_processes_all_drawers`, which never
/// asserts on `in-room` triples for back-filled drawers.
fn room_id_to_label(_room_id: uuid::Uuid) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Pre-seed the process-wide shared embedder with `MockEmbedder`.
    ///
    /// Why: both tests below seed fixture drawers via `memory_remember`, which
    /// resolves `retrieval::shared_embedder()` — a process-wide `OnceCell`
    /// where the first caller wins for the rest of the process. Under
    /// `cargo test` a sibling test's seed silently satisfied these two; under
    /// per-test process isolation (`cargo nextest run`) each gets a virgin
    /// cell, reaches for the real ONNX model, and fails on the HuggingFace
    /// download (HTTP 429 in CI). Same defect class as #4413: passing only
    /// because a sibling ran first. Neither test builds state through a shared
    /// fixture, so each establishes the precondition itself.
    /// What: delegates to `seed_shared_embedder_with_mock`, which is idempotent
    /// (`OnceCell::set`), so order does not matter.
    /// Test: `kg_rebuild_processes_all_drawers`,
    ///       `kg_rebuild_processes_named_palace_only`.
    fn seed_embedder() {
        trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();
    }

    /// Build an active triple for the purge-selection tests.
    ///
    /// Why: only subject and provenance drive selection; the rest is noise the
    /// fixtures should not have to restate.
    /// What: an active (`valid_to: None`) triple with the given subject and
    /// provenance.
    /// Test: used by the purge selection tests below.
    fn active_triple(subject: &str, provenance: Option<&str>) -> Triple {
        Triple {
            subject: subject.to_string(),
            predicate: "is-a".to_string(),
            object: "thing".to_string(),
            valid_from: chrono::Utc::now(),
            valid_to: None,
            confidence: 0.6,
            provenance: provenance.map(|p| p.to_string()),
        }
    }

    /// Why: the purge deletes real data, so selection must pick exactly the
    /// #4678 garbage and nothing adjacent to it.
    /// What: stopword and too-short auto-extracted subjects are selected;
    /// ordinary entity subjects are not, whatever their provenance.
    /// Test: This test.
    #[test]
    fn purge_selects_only_stopword_subjects() {
        let active = vec![
            active_triple("them", Some(AUTO_PROVENANCE)),
            active_triple("the", Some(AUTO_PROVENANCE)),
            active_triple("x", Some(AUTO_PROVENANCE)),
            active_triple("rustc", Some(AUTO_PROVENANCE)),
            active_triple("trusty-memory", Some(AUTO_PROVENANCE)),
            active_triple("go", Some(AUTO_PROVENANCE)),
        ];
        let got = stale_subject_candidates(&active);
        assert_eq!(
            got,
            vec!["the".to_string(), "them".to_string(), "x".to_string()],
            "selection must be the stopword/short subjects only, sorted"
        );
    }

    /// Why: `tag:the` is a tag the user wrote, not extraction debris, and
    /// `delete_by_subject` would take every membership edge with it.
    /// What: subjects under the drawer/tag/topic/room namespaces are skipped
    /// even when their local part is a stopword.
    /// Test: This test.
    #[test]
    fn purge_skips_namespaced_subjects() {
        let active = vec![
            active_triple("tag:the", Some(AUTO_PROVENANCE)),
            active_triple("topic:it", Some(AUTO_PROVENANCE)),
            active_triple("room:a", Some(AUTO_PROVENANCE)),
            active_triple(
                &format!("{DRAWER_SUBJECT_PREFIX}{}", uuid::Uuid::new_v4()),
                Some(AUTO_PROVENANCE),
            ),
        ];
        assert!(
            stale_subject_candidates(&active).is_empty(),
            "namespaced subjects must never be purged"
        );
    }

    /// Why: the #4678 `first_token` fix cleans tokens on the way IN, so it only
    /// helps content extracted from now on. Subjects already in redb were
    /// written by the old extractor with the punctuation welded on. If
    /// `is_stop_token` stopped normalising, those would stop being recognised
    /// and the purge would walk straight past the rows it exists to remove.
    /// What: a stored `("the` subject is still selected.
    /// Test: This test.
    #[test]
    fn purge_selects_a_legacy_subject_with_welded_punctuation() {
        let active = vec![
            active_triple("(\"the", Some(AUTO_PROVENANCE)),
            active_triple("`it`", Some(AUTO_PROVENANCE)),
            active_triple("`redb`", Some(AUTO_PROVENANCE)),
        ];
        assert_eq!(
            stale_subject_candidates(&active),
            vec!["(\"the".to_string(), "`it`".to_string()],
            "legacy punctuated stopword subjects must still be selected, \
             and a legacy punctuated real entity must not be"
        );
    }

    /// Why: `delete_by_subject` closes EVERY active triple under the subject,
    /// so one hand-asserted fact must protect the whole subject — a purge is
    /// not allowed to take a manual assertion with it.
    /// What: a stopword subject carrying one non-`auto:remember` triple is not
    /// selected; the same subject with only auto triples is.
    /// Test: This test.
    #[test]
    fn purge_spares_a_subject_with_a_manual_triple() {
        let mixed = vec![
            active_triple("them", Some(AUTO_PROVENANCE)),
            active_triple("them", Some("kg_assert")),
        ];
        assert!(
            stale_subject_candidates(&mixed).is_empty(),
            "a manual triple must protect the subject"
        );

        let unprovenanced = vec![active_triple("them", None)];
        assert!(
            stale_subject_candidates(&unprovenanced).is_empty(),
            "an unstamped triple is not provably auto-extracted"
        );

        let auto_only = vec![active_triple("them", Some(AUTO_PROVENANCE))];
        assert_eq!(
            stale_subject_candidates(&auto_only),
            vec!["them".to_string()]
        );
    }

    /// Why: the purge must never fire as a side effect of an ordinary rebuild.
    /// What: the default options carry both switches off, so
    /// `handle_kg_rebuild`'s one-argument form cannot delete anything.
    /// Test: This test.
    #[test]
    fn purge_is_off_by_default() {
        let opts = KgRebuildOptions::default();
        assert!(!opts.purge_stale_subjects, "purge must be opt-in");
        assert!(!opts.dry_run);
    }

    /// Why: the selection tests are pure; this one proves the purge actually
    /// reaches `delete_by_subject` and that `--dry-run` writes nothing — the
    /// two claims that matter when the flag is pointed at a real palace.
    /// What: seeds `them --is-a--> no-op` (the live #4678 triple) plus a real
    /// `rustc --is-a--> compiler` into a tempdir palace. A dry run lists the
    /// stale subject and leaves both in place; an applied run closes the stale
    /// one and leaves the real one untouched.
    /// Test: This test.
    #[tokio::test]
    async fn purge_deletes_stale_subjects_only_when_applied() -> Result<()> {
        seed_embedder();
        let tmp = tempfile::tempdir()?;
        // Issue #88: bypass palace-slug enforcement for test palaces.
        // SAFETY: idempotent constant write "1"; safe across test threads.
        unsafe {
            std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
        }
        let state = AppState::new(tmp.path().to_path_buf());
        state.set_ready();
        let _ = crate::tools::dispatch_tool(&state, "palace_create", json!({"name": "a"})).await?;

        let handle = state
            .registry
            .open_palace(&state.data_root, &PalaceId::new("a"))?;
        handle
            .kg
            .assert(active_triple("them", Some(AUTO_PROVENANCE)))
            .await?;
        handle
            .kg
            .assert(active_triple("rustc", Some(AUTO_PROVENANCE)))
            .await?;

        // Dry run: reports the stale subject, deletes nothing.
        let dry = purge_palaces(&state, Some("a"), false).await?;
        assert_eq!(dry.len(), 1);
        assert_eq!(dry[0].subjects, vec!["them".to_string()]);
        assert_eq!(dry[0].rows_closed, 0, "a dry run must not close any row");
        assert!(
            !handle.kg.query_active("them").await?.is_empty(),
            "dry run must leave the stale triple in place"
        );

        // Applied: closes the stale subject and spares the real one.
        let applied = purge_palaces(&state, Some("a"), true).await?;
        assert_eq!(applied[0].subjects, vec!["them".to_string()]);
        assert!(
            applied[0].rows_closed > 0,
            "applied purge must close at least one row"
        );
        assert!(
            handle.kg.query_active("them").await?.is_empty(),
            "applied purge must remove the stale triple"
        );
        assert!(
            !handle.kg.query_active("rustc").await?.is_empty(),
            "applied purge must not touch a real entity subject"
        );
        Ok(())
    }

    /// Why: Validate the back-fill end-to-end against a freshly-created
    /// palace with a known drawer count.
    /// What: Build a tempdir-rooted `AppState`, create two palaces, drop a
    /// drawer in each via `dispatch_tool("memory_remember", ...)`, run
    /// `rebuild_palaces(None)`, and confirm both palaces show up with the
    /// expected drawer counts.
    /// Test: This test.
    #[tokio::test]
    async fn kg_rebuild_processes_all_drawers() -> Result<()> {
        seed_embedder();
        let tmp = tempfile::tempdir()?;
        // Issue #88: bypass palace-slug enforcement for test palaces.
        // SAFETY: tests using TRUSTY_SKIP_PALACE_ENFORCEMENT set a constant
        // value "1"; idempotent across concurrent test threads.
        unsafe {
            std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
        }
        let state = AppState::new(tmp.path().to_path_buf());
        // Flip to Ready so the issue #911 readiness preflight does not block
        // the memory_remember calls that seed fixture drawers.
        state.set_ready();

        // Create two palaces, one drawer each.
        let _ = crate::tools::dispatch_tool(&state, "palace_create", json!({"name": "a"})).await?;
        let _ = crate::tools::dispatch_tool(&state, "palace_create", json!({"name": "b"})).await?;
        let _ = crate::tools::dispatch_tool(
            &state,
            "memory_remember",
            json!({
                "palace": "a",
                // 8+ tokens to clear the MCP min-token gate. Content includes
                // an `is a` pattern hit so the back-fill produces at least
                // one non-tag triple.
                "text": "The Rustc compiler is a fast tool for the Rust language",
                "tags": ["compiler"],
                "room": "Backend",
            }),
        )
        .await?;
        let _ = crate::tools::dispatch_tool(
            &state,
            "memory_remember",
            json!({
                "palace": "b",
                "text": "Cargo build is a tool that compiles every #rust crate",
                "tags": ["tooling"],
            }),
        )
        .await?;

        let summaries = rebuild_palaces(&state, None).await?;
        assert_eq!(summaries.len(), 2, "expected both palaces processed");
        for s in &summaries {
            assert!(
                s.error.is_none(),
                "palace {} errored: {:?}",
                s.palace_id,
                s.error
            );
            assert_eq!(
                s.drawers_scanned, 1,
                "palace {} expected one drawer",
                s.palace_id
            );
            assert!(
                s.triples_asserted > 0,
                "palace {} expected non-zero triples",
                s.palace_id
            );
        }
        Ok(())
    }

    /// Why: The `--palace` flag narrows the rebuild to a single palace; the
    /// caller must not pay for unrelated palaces.
    /// What: Same fixture as the previous test, but call
    /// `rebuild_palaces(Some("a"))` and confirm only palace `a` shows up.
    /// Test: This test.
    #[tokio::test]
    async fn kg_rebuild_processes_named_palace_only() -> Result<()> {
        seed_embedder();
        let tmp = tempfile::tempdir()?;
        // Issue #88: bypass palace-slug enforcement for test palaces.
        // SAFETY: idempotent constant write "1"; safe across test threads.
        unsafe {
            std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
        }
        let state = AppState::new(tmp.path().to_path_buf());
        // Flip to Ready so the issue #911 readiness preflight does not block
        // the memory_remember calls that seed fixture drawers.
        state.set_ready();

        let _ = crate::tools::dispatch_tool(&state, "palace_create", json!({"name": "a"})).await?;
        let _ = crate::tools::dispatch_tool(&state, "palace_create", json!({"name": "b"})).await?;
        let _ = crate::tools::dispatch_tool(
            &state,
            "memory_remember",
            json!({
                "palace": "a",
                "text": "The Rustc compiler is a fast tool for Rust language users",
                "tags": ["compiler"],
            }),
        )
        .await?;
        let _ = crate::tools::dispatch_tool(
            &state,
            "memory_remember",
            json!({
                "palace": "b",
                "text": "Cargo build is a tool that compiles every Rust crate locally",
                "tags": ["tooling"],
            }),
        )
        .await?;

        let summaries = rebuild_palaces(&state, Some("a")).await?;
        assert_eq!(summaries.len(), 1, "only palace 'a' should be processed");
        assert_eq!(summaries[0].palace_id, "a");
        Ok(())
    }
}
