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
use trusty_common::memory_core::store::kg::{KnowledgeGraph, Triple};
use trusty_common::memory_core::store::OpenIntent;

use super::kg_twin_merge::report_merge;
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
pub(crate) const STRUCTURAL_PREFIXES: &[&str] = &[
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
/// logged without aborting the rest of the run). `error` is `None` only when
/// nothing failed — a palace that could not be opened AND a palace whose
/// individual `assert` calls failed both set it (#5531), so `None` can never
/// stand in for "nothing was examined".
/// Test: `kg_rebuild_processes_all_drawers` asserts the field values;
/// `rebuild_reports_failed_asserts_instead_of_a_clean_summary` covers the
/// per-triple error arm.
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
/// What: the palace filter plus the maintenance switches — #4678's purge,
/// #5401's twin merge, and the shared dry run. `Default` is an ordinary rebuild
/// of every palace, which is the pre-#4678 behaviour.
/// Test: `purge_is_off_by_default`.
#[derive(Debug, Clone, Default)]
pub struct KgRebuildOptions {
    /// Restrict the run to a single palace id. `None` processes every palace.
    pub palace: Option<String>,
    /// Delete auto-extracted subjects the #4678 token filter now rejects.
    pub purge_stale_subjects: bool,
    /// Re-point auto-extracted triples off a punctuated entity node onto its
    /// cleaned twin (#5401).
    pub merge_punctuated_twins: bool,
    /// Report what the selected maintenance passes would do and write nothing.
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
/// with `merge_punctuated_twins` it re-points each punctuated entity's triples
/// onto the cleaned twin (#5401); with `dry_run` it skips the rebuild entirely
/// and only reports what the selected passes would do, so the whole invocation
/// writes nothing.
/// Test: not unit-tested (process-level entry point); `rebuild_palaces` and
/// `purge_palaces` are the testable surfaces.
pub async fn handle_kg_rebuild_with(opts: KgRebuildOptions) -> Result<()> {
    let data_dir = trusty_common::resolve_data_dir("trusty-memory")
        .context("resolve trusty-memory data dir")?;
    let data_root = resolve_palace_registry_dir(data_dir);
    let state = AppState::new(data_root);
    let palace = opts.palace.clone();

    // #4678: the dry-run branch returns before ANY palace is hydrated.
    // `load_palaces_from_disk` opens every palace through `PalaceHandle::open`,
    // whose issue-#61 reclamation sweep hard-deletes expired non-Tier-C
    // drawers — a write, and precisely the thing --dry-run promises not to do.
    if opts.dry_run {
        println!("kg-rebuild: DRY RUN — nothing is written: no palace is hydrated, no triple is asserted, no subject is deleted");
        let mut failures = 0usize;
        if opts.purge_stale_subjects {
            failures += report_purge(&state, palace.as_deref(), false).await?;
        }
        if opts.merge_punctuated_twins {
            failures += report_merge(&state, palace.as_deref(), false).await?;
        }
        if failures > 0 {
            anyhow::bail!("kg-rebuild: {failures} palace(s) could not be scanned");
        }
        return Ok(());
    }

    // #4911: the applying pass ASSERTS triples through the handles it opens, so
    // it must hold `Writer` intent. A `ReadOnlyClient` registry silently serves
    // a snapshot when the daemon holds the lock, and every `kg.assert` against
    // it fails into `rebuild_one`'s non-fatal warn arm — the run reports success
    // having written nothing. Same reasoning `purge_one` already applies to its
    // applying pass, and the same fail-loud direction as #1487.
    //
    // The `load_palaces_from_disk` hydration this replaces was redundant AND
    // defeated the intent: it opens every palace with the zero-arg
    // `PalaceHandle::open` (`ReadOnlyClient`) and registers it, so `rebuild_one`
    // would then hit those cached read-only handles. `rebuild_palaces`
    // enumerates from disk (`PalaceRegistry::list_palaces`), never from the
    // handle cache, so dropping the pre-open changes nothing it can observe —
    // each palace is opened once, lazily, through the writer-intent registry.
    let state = state.with_writer_intent();

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
        let failures = report_purge(&state, palace.as_deref(), true).await?;
        if failures > 0 {
            anyhow::bail!(
                "kg-rebuild purge: {failures} subject deletion(s) failed — see the [purge-FAILED] lines above"
            );
        }
    }
    // #5401: merge after the purge — the purge deletes the stopword subjects
    // this pass is required to leave alone, so a merged palace never inherits
    // one of them as a target.
    if opts.merge_punctuated_twins {
        let failures = report_merge(&state, palace.as_deref(), true).await?;
        if failures > 0 {
            anyhow::bail!(
                "kg-rebuild merge: {failures} re-point(s) failed — see the [merge-FAILED] lines above"
            );
        }
    }
    Ok(())
}

/// Print every stale subject, then optionally delete it.
///
/// Why: the purge is destructive against real data, so the subject list is
/// printed in both modes — an operator sees exactly what was (or would be)
/// removed rather than a bare count.
/// What: runs [`purge_palaces`] and prints DELETED and FAILED subjects on
/// separate, differently-tagged channels — deletions on stdout, failures on
/// stderr — then an aggregate carrying the failure count. Returns that count so
/// the caller can exit non-zero. `apply` selects deletion versus report-only.
/// Test: `purge_reports_a_failed_delete_as_failed_not_deleted` covers the
/// outcome split this prints.
async fn report_purge(state: &AppState, palace_filter: Option<&str>, apply: bool) -> Result<usize> {
    let summaries = purge_palaces(state, palace_filter, apply).await?;
    let mut total_subjects = 0usize;
    let mut total_rows = 0usize;
    let failures = count_purge_failures(&summaries);
    for s in &summaries {
        if let Some(e) = &s.error {
            eprintln!("[purge-error] palace={} error={}", s.palace_id, e);
            // A palace that could not be opened or scanned has nothing
            // per-subject to print.
            if s.failed.is_empty() {
                continue;
            }
        }
        if apply {
            for subject in &s.deleted {
                println!("[purge] palace={} deleted subject={}", s.palace_id, subject);
            }
            for (subject, err) in &s.failed {
                eprintln!(
                    "[purge-FAILED] palace={} subject={} error={}",
                    s.palace_id, subject, err
                );
            }
            total_subjects += s.deleted.len();
        } else {
            for subject in &s.selected {
                println!(
                    "[purge] palace={} would delete subject={}",
                    s.palace_id, subject
                );
            }
            total_subjects += s.selected.len();
        }
        total_rows += s.rows_closed;
    }
    if apply {
        println!(
            "kg-rebuild purge: {total_subjects} subjects deleted, {total_rows} rows closed, {failures} failed"
        );
    } else {
        println!("kg-rebuild purge: {total_subjects} subjects would be deleted (dry run)");
    }
    Ok(failures)
}

/// How many failures a purge run must report.
///
/// Why: the count drives the `anyhow::bail!` that gives the command its
/// non-zero exit, so it is the operator-visible half of CRITICAL 1. It also
/// carries the one piece of arithmetic that is easy to get wrong: a palace with
/// per-subject failures ALSO has `error` set (it summarises those same
/// failures), so counting both would report every such palace one time too many.
/// Pulled out of `report_purge` so both arms can be tested directly — a
/// per-subject delete failure is not reachable deterministically through the
/// real pipeline, because an applying run opens with `Writer` intent and
/// therefore either opens and deletes cleanly or fails at the open.
/// What: per palace, per-subject failures when there are any, otherwise one for
/// a palace-level `error`, otherwise zero. Never both.
/// Test: `count_purge_failures_never_double_counts_a_palace`,
/// `purge_counts_an_unopenable_palace_as_exactly_one_failure`.
pub fn count_purge_failures(summaries: &[PalacePurgeSummary]) -> usize {
    summaries
        .iter()
        .map(|s| {
            if !s.failed.is_empty() {
                s.failed.len()
            } else if s.error.is_some() {
                1
            } else {
                0
            }
        })
        .sum()
}

/// Per-palace result of a `--purge-stale-subjects` pass.
///
/// Why: mirrors [`PalaceRebuildSummary`] so one bad palace is reported without
/// aborting the rest of the run.
/// What: `selected` is what the scan picked; `deleted` and `failed` split what
/// actually happened, so a caller can never read a failure as a success.
/// `rows_closed` counts only rows a SUCCEEDING delete closed, and `error` is
/// set whenever anything failed — the failure is therefore visible without
/// reading a tracing log.
/// Test: `purge_selects_only_stopword_subjects`,
/// `purge_reports_a_failed_delete_as_failed_not_deleted`.
#[derive(Debug, Clone)]
pub struct PalacePurgeSummary {
    pub palace_id: String,
    /// Subjects the scan picked as purge candidates.
    pub selected: Vec<String>,
    /// Subjects whose delete returned `Ok`. Empty in report-only mode.
    pub deleted: Vec<String>,
    /// Subjects whose delete returned `Err`, with the error rendered.
    pub failed: Vec<(String, String)>,
    pub rows_closed: usize,
    pub error: Option<String>,
}

/// Result of running the deletions for one palace.
///
/// Why: a delete that fails must not be counted, printed, or summarised as one
/// that succeeded — the whole of CRITICAL 1. Keeping the three outputs in one
/// value makes it impossible to update the count without also recording which
/// side the subject landed on.
/// What: succeeded subjects, failed subjects with their error text, and the
/// rows closed by the successes only.
/// Test: `purge_reports_a_failed_delete_as_failed_not_deleted`.
#[derive(Debug, Clone, Default)]
pub struct PurgeOutcome {
    pub deleted: Vec<String>,
    pub failed: Vec<(String, String)>,
    pub rows_closed: usize,
}

/// Run `delete` over every subject, recording each outcome separately.
///
/// Why: the deletion loop is the one place a fail-open branch could report a
/// failure as a success, and it needs an error-arm test. Taking the delete as a
/// closure gives that test a seam — it can fail a chosen subject deterministically
/// without needing a read-only store or a live lock contender.
/// What: calls `delete` per subject; `Ok(n)` appends to `deleted` and adds `n`
/// to `rows_closed`, `Err` appends `(subject, error)` to `failed` and adds
/// nothing. Never short-circuits — one bad subject must not strand the rest.
/// Test: `purge_reports_a_failed_delete_as_failed_not_deleted`.
pub fn apply_deletions<F>(subjects: &[String], mut delete: F) -> PurgeOutcome
where
    F: FnMut(&str) -> Result<usize>,
{
    let mut out = PurgeOutcome::default();
    for subject in subjects {
        match delete(subject) {
            Ok(n) => {
                out.rows_closed += n;
                out.deleted.push(subject.clone());
            }
            Err(e) => out.failed.push((subject.clone(), format!("{e:#}"))),
        }
    }
    out
}

/// Select — and optionally delete — stale auto-extracted subjects.
///
/// Why: #4678's forward filter only stops NEW garbage. `rebuild_one` re-asserts
/// and never retracts, and the four pattern predicates are absent from
/// `FUNCTIONAL_PREDICATES`, so `assert` supersedes only an identical object.
/// Without this pass every triple already in the graph stays there forever.
///
/// Error containment stops at the palace: a registry read that fails yields no
/// palaces to contain, and swallowing it printed `0 subjects deleted` over data
/// the pass never read. A TCC denial on the data dir makes that `read_dir`
/// return EPERM, so the clean-looking exit-0 is reachable, not theoretical.
/// What: per palace, scans every active triple, picks the subjects
/// [`stale_subject_candidates`] returns, and (when `apply`) calls the existing
/// `delete_by_subject` on each. A failing palace is captured as a summary
/// carrying `error`; a failure to list the palaces at all propagates instead.
/// Test: `purge_selects_only_stopword_subjects`, `purge_skips_namespaced_subjects`,
/// `purge_palaces_propagates_an_unreadable_data_root`.
pub async fn purge_palaces(
    state: &AppState,
    palace_filter: Option<&str>,
    apply: bool,
) -> Result<Vec<PalacePurgeSummary>> {
    let mut out: Vec<PalacePurgeSummary> = Vec::new();
    // #5511: an unreadable data root is a failed purge, never an empty one.
    let palaces = trusty_common::memory_core::PalaceRegistry::list_palaces(&state.data_root)
        .with_context(|| format!("list palaces under {}", state.data_root.display()))?;
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
                selected: Vec::new(),
                deleted: Vec::new(),
                failed: Vec::new(),
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
/// What: opens the palace's `kg.db` DIRECTLY rather than through
/// `PalaceRegistry::open_palace`, pages `list_active`, runs
/// [`stale_subject_candidates`], then (when `apply`) hands the matches to
/// [`apply_deletions`] on the blocking pool. The in-memory adjacency is not
/// resynced — this runs in a one-shot CLI process that exits immediately after.
///
/// The direct open is the #4678 fix for the dry run that wrote:
/// `PalaceHandle::open` runs the issue-#61 expired-drawer sweep, which
/// hard-deletes every expired non-Tier-C row before the caller ever sees the
/// handle. `KnowledgeGraph::open_with_intent` only opens, runs the (no-op)
/// legacy migration, and hydrates the adjacency, so a scan cannot mutate what
/// it is reporting on. Intent follows the mode: a report-only pass asks for
/// `ReadOnlyClient`, an applying pass asks for `Writer` so it fails loud
/// against a running daemon rather than silently deleting from a snapshot.
/// Test: `purge_selects_only_stopword_subjects`,
/// `purge_dry_run_does_not_prune_expired_drawers`.
async fn purge_one(state: &AppState, palace_id: &str, apply: bool) -> Result<PalacePurgeSummary> {
    let kg_path = state.data_root.join(palace_id).join("kg.db");
    let intent = if apply {
        OpenIntent::Writer
    } else {
        OpenIntent::ReadOnlyClient
    };
    let kg = KnowledgeGraph::open_with_intent(&kg_path, intent)
        .with_context(|| format!("open kg for palace {palace_id}"))?;

    let active = scan_active_triples(&kg, palace_id).await?;
    let selected = stale_subject_candidates(&active);
    let outcome = if apply {
        let store = kg.store();
        let subjects = selected.clone();
        tokio::task::spawn_blocking(move || {
            apply_deletions(&subjects, |s| store.delete_by_subject(s))
        })
        .await
        .with_context(|| format!("purge delete task for palace {palace_id}"))?
    } else {
        PurgeOutcome::default()
    };

    let error = if outcome.failed.is_empty() {
        None
    } else {
        Some(format!(
            "{} of {} selected subject(s) failed to delete",
            outcome.failed.len(),
            selected.len()
        ))
    };
    Ok(PalacePurgeSummary {
        palace_id: palace_id.to_string(),
        selected,
        deleted: outcome.deleted,
        failed: outcome.failed,
        rows_closed: outcome.rows_closed,
        error,
    })
}

/// Page every active triple in `kg` into one vec.
///
/// Why: both maintenance passes — the #4678 purge and the #5401 twin merge —
/// decide from the whole active set, and `list_active` is a windowed read. One
/// scan means the page size and the last-page condition cannot drift between
/// them.
/// What: repeats `list_active` at [`PURGE_SCAN_PAGE`] until a short page ends
/// the walk.
/// Test: `purge_selects_only_stopword_subjects` (through `purge_one`),
/// `merge_repoints_both_positions_and_keeps_the_cleaned_nodes_triples`.
pub(crate) async fn scan_active_triples(
    kg: &KnowledgeGraph,
    palace_id: &str,
) -> Result<Vec<Triple>> {
    let mut active: Vec<Triple> = Vec::new();
    let mut offset = 0usize;
    loop {
        let page = kg
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
    Ok(active)
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
///
/// This pass only asserts, so a swallowed registry read costs less than the
/// purge's does — nothing is deleted, and the operator is merely told
/// `0 drawers, 0 triples` about a back-fill that never opened a palace. It is
/// still a lie about work not done, and the same TCC denial produces it, so it
/// propagates for the same reason `purge_palaces` does.
/// What: When `palace_filter` is `Some`, processes only the matching palace;
/// otherwise iterates every loaded palace via `PalaceRegistry::list_palaces`.
/// Each palace is processed inside its own `rebuild_one` call so a single
/// failure is captured per-palace rather than aborting the run; a failure to
/// list the palaces at all propagates instead.
/// Test: `kg_rebuild_processes_all_drawers`,
/// `kg_rebuild_processes_named_palace_only`,
/// `rebuild_palaces_propagates_an_unreadable_data_root`.
pub async fn rebuild_palaces(
    state: &AppState,
    palace_filter: Option<&str>,
) -> Result<Vec<PalaceRebuildSummary>> {
    let mut out: Vec<PalaceRebuildSummary> = Vec::new();
    // #5511: an unreadable data root is a failed back-fill, never an empty one.
    let palaces = trusty_common::memory_core::PalaceRegistry::list_palaces(&state.data_root)
        .with_context(|| format!("list palaces under {}", state.data_root.display()))?;
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
///
/// Error contract (#5531): a failed `assert` never aborts the remaining
/// drawers, but it must not vanish either. It used to be logged and nothing
/// else — the summary came back `error: None` however many asserts failed, so
/// `error: None` could mean "every triple landed" or "not one did", and the
/// operator read `[ok]` over a back-fill that wrote nothing. The producer of
/// that state is real: with a running daemon holding the redb write lock a
/// `ReadOnlyClient` open serves a snapshot, and every `assert` against it is
/// rejected (#4911).
/// What: Opens the palace handle, snapshots the drawer table, runs
/// `extract_triples` on each drawer, and calls `handle.kg.assert` for every
/// result. Each failure is counted and the first one is kept for the message;
/// a non-zero count sets `error` to `N of M ... failed to assert`, exactly as
/// `purge_one` reports its failed deletes. `Err` is still reserved for a hard
/// failure to open the palace or read the drawer list. The exit code is
/// unchanged — `handle_kg_rebuild_with` prints the `[error]` line and counts
/// it, and gating the process exit on it is a separate decision (#5531).
/// Test: `kg_rebuild_processes_all_drawers` (drawer count and asserted count
/// must match the heuristic expectations),
/// `rebuild_reports_failed_asserts_instead_of_a_clean_summary` (the error arm).
async fn rebuild_one(state: &AppState, palace_id: &str) -> Result<PalaceRebuildSummary> {
    let pid = PalaceId::new(palace_id);
    let handle = state
        .registry
        .open_palace(&state.data_root, &pid)
        .with_context(|| format!("open palace {palace_id}"))?;

    let drawers = handle.drawers.read().clone();
    let mut asserted = 0usize;
    // #5531: a failed assert has to reach the summary, not only the log.
    // Counts plus the first failure keep the report bounded on a palace whose
    // every triple fails.
    let mut attempted = 0usize;
    let mut failed = 0usize;
    let mut first_failure: Option<String> = None;
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
            attempted += 1;
            match handle.kg.assert(triple).await {
                Ok(()) => asserted += 1,
                Err(e) => {
                    failed += 1;
                    if first_failure.is_none() {
                        first_failure = Some(format!("{s} --{p}-->: {e:#}"));
                    }
                    tracing::warn!(
                        palace = %palace_id,
                        drawer_id = %d.id,
                        subject = %s,
                        predicate = %p,
                        "kg-rebuild: assert failed (non-fatal): {e:#}",
                    );
                }
            }
        }
    }
    // `first_failure` is Some exactly when `failed` is non-zero, so the map
    // carries the "did anything fail" test and the message in one step.
    let error = first_failure.map(|first| {
        format!("{failed} of {attempted} extracted triple(s) failed to assert; first: {first}")
    });
    Ok(PalaceRebuildSummary {
        palace_id: palace_id.to_string(),
        drawers_scanned: drawers.len(),
        triples_asserted: asserted,
        error,
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

    /// Why: neither maintenance pass may fire as a side effect of an ordinary
    /// rebuild.
    /// What: the default options carry every switch off, so
    /// `handle_kg_rebuild`'s one-argument form cannot delete or re-point
    /// anything.
    /// Test: This test.
    #[test]
    fn purge_is_off_by_default() {
        let opts = KgRebuildOptions::default();
        assert!(!opts.purge_stale_subjects, "purge must be opt-in");
        assert!(!opts.merge_punctuated_twins, "the merge must be opt-in");
        assert!(!opts.dry_run);
    }

    /// Why: the deletion loop downgraded a failed `delete_by_subject` to a
    /// `tracing::warn!` and carried on, then returned the unfiltered candidate
    /// list as the result with `error: None`. The report printed
    /// `deleted subject=X` for a subject still in the graph and counted it in
    /// the total, so the only trace of a failure was a log line an operator had
    /// no reason to read. A purge that cannot delete must not claim it did.
    /// What: with one subject's delete failing, that subject appears in
    /// `failed` with its error text and NOT in `deleted`, and its rows are not
    /// added to `rows_closed`. The loop still completes the other subjects.
    /// Test: This test.
    #[test]
    fn purge_reports_a_failed_delete_as_failed_not_deleted() {
        let subjects = vec!["them".to_string(), "the".to_string(), "it".to_string()];
        let outcome = apply_deletions(&subjects, |s| {
            if s == "the" {
                anyhow::bail!("store is read-only")
            } else {
                Ok(2)
            }
        });
        assert_eq!(
            outcome.deleted,
            vec!["them".to_string(), "it".to_string()],
            "only subjects whose delete returned Ok may be reported as deleted"
        );
        assert_eq!(outcome.failed.len(), 1, "the failing subject must be kept");
        assert_eq!(outcome.failed[0].0, "the");
        assert!(
            outcome.failed[0].1.contains("store is read-only"),
            "the failure must carry its error text, got {:?}",
            outcome.failed[0].1
        );
        assert_eq!(
            outcome.rows_closed, 4,
            "a failed delete must contribute no closed rows"
        );
    }

    /// Build a summary for the counting tests.
    fn summary_of(
        palace_id: &str,
        selected: &[&str],
        deleted: &[&str],
        failed: &[(&str, &str)],
        error: Option<&str>,
    ) -> PalacePurgeSummary {
        PalacePurgeSummary {
            palace_id: palace_id.to_string(),
            selected: selected.iter().map(|s| s.to_string()).collect(),
            deleted: deleted.iter().map(|s| s.to_string()).collect(),
            failed: failed
                .iter()
                .map(|(s, e)| (s.to_string(), e.to_string()))
                .collect(),
            rows_closed: deleted.len(),
            error: error.map(|e| e.to_string()),
        }
    }

    /// Why: a palace with per-subject failures ALSO carries a palace-level
    /// `error` summarising those same failures. Counting both would report one
    /// extra failure for every such palace, inflating the number the operator
    /// sees and the one the non-zero exit is justified by.
    /// What: per-subject failures count once each; a palace-level error counts
    /// once ONLY when there are no per-subject failures behind it; a clean
    /// palace counts zero.
    /// Test: This test.
    #[test]
    fn count_purge_failures_never_double_counts_a_palace() {
        let open_failed = summary_of("a", &[], &[], &[], Some("open kg for palace a: bad file"));
        let subject_failed = summary_of(
            "b",
            &["them", "the"],
            &["them"],
            &[("the", "store is read-only")],
            Some("1 of 2 selected subject(s) failed to delete"),
        );
        let clean = summary_of("c", &["it"], &["it"], &[], None);

        assert_eq!(
            count_purge_failures(std::slice::from_ref(&open_failed)),
            1,
            "a palace that could not be opened is one failure"
        );
        assert_eq!(
            count_purge_failures(std::slice::from_ref(&subject_failed)),
            1,
            "the error merely summarises the one subject failure; it must not add a second"
        );
        assert_eq!(count_purge_failures(std::slice::from_ref(&clean)), 0);
        assert_eq!(
            count_purge_failures(&[open_failed, subject_failed, clean]),
            2,
            "aggregate must be 1 + 1 + 0"
        );
    }

    /// Why: `report_purge` is what turns a failure into something the operator
    /// can see — the stderr line, the count in the aggregate, and the value
    /// that drives the non-zero exit. `apply_deletions` covers the injected
    /// closure; this drives a REAL failure through
    /// `purge_palaces` → `purge_one` → `report_purge`.
    /// What: corrupts the palace's `kg.redb` so `KnowledgeGraph::open_with_intent`
    /// fails, then asserts the summary records it as a palace-level error with
    /// no per-subject entries, and that `report_purge` counts it exactly once.
    /// Deterministic: the failure is a malformed file, not a lock race.
    /// Test: This test.
    #[tokio::test]
    async fn purge_counts_an_unopenable_palace_as_exactly_one_failure() -> Result<()> {
        seed_embedder();
        let tmp = tempfile::tempdir()?;
        let data_root = tmp.path().to_path_buf();
        // SAFETY: idempotent constant write "1"; safe across test threads.
        unsafe {
            std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
        }
        {
            let state = AppState::new(data_root.clone());
            state.set_ready();
            let _ =
                crate::tools::dispatch_tool(&state, "palace_create", json!({"name": "a"})).await?;
        }

        // Build a SECOND palace on disk that this process never opens, by
        // cloning the created palace's metadata. The store keeps a
        // process-wide cache of open databases keyed by path, so a palace
        // already opened in this test would be served from that cache and
        // never touch the filesystem at all — an earlier version of this
        // fixture corrupted `a/kg.redb` and the open still succeeded.
        let mut record: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(data_root.join("a/palace.json"))?)?;
        if let Some(obj) = record.as_object_mut() {
            obj.insert("id".to_string(), json!("b"));
            if obj.contains_key("name") {
                obj.insert("name".to_string(), json!("b"));
            }
        }
        let b_dir = data_root.join("b");
        std::fs::create_dir_all(&b_dir)?;
        std::fs::write(b_dir.join("palace.json"), serde_json::to_string(&record)?)?;
        // A DIRECTORY where the store file belongs. Opening a directory as a
        // database file fails at the OS layer on every platform and every redb
        // version, and cannot be silently recreated underneath us. No lock, no
        // race, no timing.
        std::fs::create_dir(b_dir.join("kg.redb"))?;

        let state = AppState::new(data_root.clone());
        let summaries = purge_palaces(&state, Some("b"), true).await?;

        assert!(
            summaries[0].error.is_some(),
            "an unopenable palace must set the palace-level error"
        );
        assert!(
            summaries[0].failed.is_empty() && summaries[0].deleted.is_empty(),
            "nothing was attempted per-subject, so both lists stay empty"
        );

        let failures = report_purge(&state, Some("b"), true).await?;
        assert_eq!(
            failures, 1,
            "an unopenable palace is exactly one failure, and a non-zero count is what makes the command exit non-zero"
        );
        Ok(())
    }

    /// Why: `--dry-run` promises it writes nothing, but the scan reached the KG
    /// through `PalaceHandle::open`, whose issue-#61 reclamation sweep
    /// hard-deletes every expired non-Tier-C drawer before the caller sees the
    /// handle. `SessionEvent` drawers carry a 7-day TTL, so a live palace
    /// always has candidates — a careful operator previewing a destructive flag
    /// with the daemon stopped got an uncontended read-write open and lost
    /// drawer rows to the preview itself.
    /// What: seeds an expired non-Tier-C drawer, runs the dry-run purge from a
    /// COLD `AppState` (the registry's warm-handle fast path skips the sweep,
    /// so a shared state would not reproduce it), and asserts the drawer row
    /// count is unchanged.
    /// Test: This test.
    #[tokio::test]
    async fn purge_dry_run_does_not_prune_expired_drawers() -> Result<()> {
        use trusty_common::memory_core::palace::{Drawer, DrawerType};
        use trusty_common::memory_core::store::kg::KnowledgeGraph;

        seed_embedder();
        let tmp = tempfile::tempdir()?;
        let data_root = tmp.path().to_path_buf();
        // SAFETY: idempotent constant write "1"; safe across test threads.
        unsafe {
            std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
        }

        let before = {
            let state = AppState::new(data_root.clone());
            state.set_ready();
            let _ =
                crate::tools::dispatch_tool(&state, "palace_create", json!({"name": "a"})).await?;
            let handle = state
                .registry
                .open_palace(&state.data_root, &PalaceId::new("a"))?;
            let mut drawer = Drawer::new(uuid::Uuid::new_v4(), "an expired session event");
            drawer.drawer_type = DrawerType::SessionEvent;
            drawer.expires_at = Some(chrono::Utc::now() - chrono::Duration::days(1));
            drawer.fact_key = None; // not Tier C, so the sweep considers it
            handle.kg.upsert_drawer(&drawer).await?;
            let n = handle.kg.load_drawers()?.len();
            drop(handle);
            n
        };
        assert!(before > 0, "fixture must seed at least one drawer");

        // Cold state: this is the CLI's own situation with the daemon stopped.
        {
            let state = AppState::new(data_root.clone());
            let summaries = purge_palaces(&state, Some("a"), false).await?;
            assert_eq!(summaries.len(), 1, "palace 'a' must be scanned");
        }

        let kg = KnowledgeGraph::open(&data_root.join("a").join("kg.db"))?;
        let after = kg.load_drawers()?.len();
        assert_eq!(
            before, after,
            "a dry run must not delete drawer rows; {before} before, {after} after"
        );
        Ok(())
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
        assert_eq!(dry[0].selected, vec!["them".to_string()]);
        assert!(dry[0].deleted.is_empty(), "a dry run deletes nothing");
        assert_eq!(dry[0].rows_closed, 0, "a dry run must not close any row");
        assert!(
            !handle.kg.query_active("them").await?.is_empty(),
            "dry run must leave the stale triple in place"
        );

        // Applied: closes the stale subject and spares the real one.
        let applied = purge_palaces(&state, Some("a"), true).await?;
        assert_eq!(applied[0].selected, vec!["them".to_string()]);
        assert_eq!(applied[0].deleted, vec!["them".to_string()]);
        assert!(applied[0].failed.is_empty(), "no delete should have failed");
        assert!(applied[0].error.is_none(), "a clean purge sets no error");
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

    /// Why: `list_palaces` was called with `unwrap_or_default()`, so a registry
    /// read that failed became zero palaces and this DESTRUCTIVE pass printed a
    /// zero-count summary and exited 0 over data it never read. A macOS TCC
    /// denial on the data dir is exactly that failure — `read_dir` returns
    /// EPERM — and the operator's evidence that the purge ran clean would be a
    /// pass that never opened a single palace. Same defect `merge_palaces`
    /// closed in #5401; this is the sibling site.
    /// What: a `data_root` whose listing fails makes the purge — and the
    /// `report_purge` that wraps it — return `Err`. A regular file stands in for
    /// the denial: both reach the same `read_dir` error arm, and this one
    /// reproduces without depending on the uid the suite runs as.
    /// Test: This test.
    #[tokio::test]
    async fn purge_palaces_propagates_an_unreadable_data_root() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let data_root = tmp.path().join("not-a-directory");
        std::fs::write(&data_root, b"")?;
        let state = AppState::new(data_root);

        for apply in [false, true] {
            let err = purge_palaces(&state, None, apply)
                .await
                .expect_err("an unlistable data root must fail the purge, not report an empty one");
            let rendered = format!("{err:#}");
            assert!(
                rendered.contains("list palaces"),
                "the error must name what could not be read, got {rendered}"
            );
        }

        report_purge(&state, None, true)
            .await
            .expect_err("the reporting wrapper must not print a clean summary either");
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

    /// Why: #5399 — every unit fixture for the noun-phrase walk passes a
    /// SINGLE LINE, but this function and `auto_extract_and_assert` both hand
    /// the extractor a whole multi-line drawer body. That gap hid a walk that
    /// crossed a line break and took its head from the next sentence. The
    /// consequence is worse here than in a unit test: the KG keeps one ACTIVE
    /// triple per `(subject, predicate)`, so a rebuild does not accumulate — it
    /// REWRITES. A regression in the walk therefore supersedes a correct stored
    /// object with a worse one, and nothing in the pipeline reports it.
    /// What: stores a two-line drawer whose second line would capture the head
    /// if the walk were unbounded, rebuilds, and asserts the active `is-a`
    /// object is the noun from the FIRST line. Against `8402bd8b` this asserted
    /// `trusty-search --is-a--> builds`.
    /// Test: This test.
    #[tokio::test]
    async fn rebuild_does_not_supersede_a_good_object_from_a_later_line() -> Result<()> {
        seed_embedder();
        let tmp = tempfile::tempdir()?;
        // SAFETY: idempotent constant write "1"; safe across test threads.
        unsafe {
            std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
        }
        let state = AppState::new(tmp.path().to_path_buf());
        state.set_ready();
        let _ = crate::tools::dispatch_tool(&state, "palace_create", json!({"name": "a"})).await?;
        let _ = crate::tools::dispatch_tool(
            &state,
            "memory_remember",
            json!({
                "palace": "a",
                "text": "trusty-search is a daemon\ncargo builds it from source",
                "tags": ["infra"],
            }),
        )
        .await?;

        let summaries = rebuild_palaces(&state, Some("a")).await?;
        assert_eq!(summaries.len(), 1);
        assert!(summaries[0].error.is_none(), "{:?}", summaries[0].error);

        let handle = state
            .registry
            .open_palace(&state.data_root, &PalaceId::new("a"))?;
        let objects: Vec<String> = handle
            .kg
            .query_active("trusty-search")
            .await?
            .into_iter()
            .filter(|t| t.predicate == "is-a")
            .map(|t| t.object)
            .collect();
        assert_eq!(
            objects,
            vec!["daemon".to_string()],
            "the active is-a object must come from the line the marker is on"
        );
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

    /// Why: #5531 — every per-triple `assert` failure was downgraded to a
    /// `tracing::warn!` and the summary still came back with `error: None`, so
    /// the operator read `[ok] palace=b drawers=1 triples=0` about a back-fill
    /// that wrote nothing. The live producer of that state is #4911's
    /// read-only snapshot: with the daemon holding the redb write lock, a
    /// `ReadOnlyClient` open serves a copy and every `assert` against it is
    /// rejected by `check_writable`.
    /// What: reproduces that exact condition — a palace whose store is locked
    /// by a live writer — and asserts the summary carries `error: Some(_)`, a
    /// non-zero `triples_failed`, and a `triples_asserted` of 0 over drawers it
    /// really did scan. Deterministic: `ReadOnlyClient` takes the snapshot
    /// branch on the first `DatabaseAlreadyOpen`, with no retry window.
    /// Test: This test.
    #[tokio::test]
    async fn rebuild_reports_failed_asserts_instead_of_a_clean_summary() -> Result<()> {
        seed_embedder();
        let tmp = tempfile::tempdir()?;
        let data_root = tmp.path().to_path_buf();
        // SAFETY: idempotent constant write "1"; safe across test threads.
        unsafe {
            std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
        }
        {
            let state = AppState::new(data_root.clone());
            state.set_ready();
            let _ =
                crate::tools::dispatch_tool(&state, "palace_create", json!({"name": "a"})).await?;
            let _ = crate::tools::dispatch_tool(
                &state,
                "memory_remember",
                json!({
                    "palace": "a",
                    "text": "The Rustc compiler is a fast tool for the Rust language",
                    "tags": ["compiler"],
                }),
            )
            .await?;
        }

        // Clone the seeded palace onto an id this process has never opened.
        // The store keeps a process-wide cache of open databases keyed by
        // path, so a palace already opened here would be served a writable
        // handle from that cache and never reach the lock at all — the same
        // trap `purge_counts_an_unopenable_palace_as_exactly_one_failure`
        // documents.
        let mut record: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(data_root.join("a/palace.json"))?)?;
        if let Some(obj) = record.as_object_mut() {
            obj.insert("id".to_string(), json!("b"));
            if obj.contains_key("name") {
                obj.insert("name".to_string(), json!("b"));
            }
        }
        let b_dir = data_root.join("b");
        // `PalaceHandle::open` takes every store path from the record's
        // `data_dir`, so the clone has to be re-rooted or it re-opens palace
        // `a`'s files and never touches the locked one.
        if let Some(obj) = record.as_object_mut() {
            obj.insert("data_dir".to_string(), json!(b_dir));
        }
        std::fs::create_dir_all(&b_dir)?;
        std::fs::write(b_dir.join("palace.json"), serde_json::to_string(&record)?)?;
        std::fs::copy(data_root.join("a/kg.redb"), b_dir.join("kg.redb"))?;

        // Stand in for the running daemon: hold the exclusive redb lock so the
        // rebuild's `ReadOnlyClient` open degrades to a read-only snapshot.
        let _daemon_lock = redb::Database::create(b_dir.join("kg.redb"))?;

        let state = AppState::new(data_root.clone());
        let summaries = rebuild_palaces(&state, Some("b")).await?;
        assert_eq!(summaries.len(), 1, "palace 'b' must be processed");
        let s = &summaries[0];
        assert!(
            s.drawers_scanned > 0,
            "fixture must hand the rebuild at least one drawer to work on"
        );
        assert_eq!(
            s.triples_asserted, 0,
            "no assert can succeed against a read-only snapshot"
        );
        let Some(error) = s.error.as_deref() else {
            panic!(
                "a rebuild whose asserts all failed must not report a clean summary; \
                 got error=None drawers={} triples={}",
                s.drawers_scanned, s.triples_asserted
            );
        };
        assert!(
            error.contains("failed to assert"),
            "the error must say what failed and how much of it, got {error:?}"
        );
        Ok(())
    }

    /// Why: `rebuild_palaces` carried the same `unwrap_or_default()` on the
    /// registry read as `purge_palaces` did. This pass only asserts, so the
    /// cost is lower — nothing is deleted — but the operator is still told
    /// `0 drawers, 0 triples` about a back-fill that never opened a palace, and
    /// the same macOS TCC denial on the data dir produces it.
    /// What: a `data_root` whose listing fails makes the back-fill return
    /// `Err`. Same stand-in as the purge's twin: a regular file reaches the
    /// `read_dir` error arm without depending on the uid the suite runs as.
    /// Test: This test.
    #[tokio::test]
    async fn rebuild_palaces_propagates_an_unreadable_data_root() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let data_root = tmp.path().join("not-a-directory");
        std::fs::write(&data_root, b"")?;
        let state = AppState::new(data_root);

        let err = rebuild_palaces(&state, None)
            .await
            .expect_err("an unlistable data root must fail the back-fill, not report an empty one");
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("list palaces"),
            "the error must name what could not be read, got {rendered}"
        );
        Ok(())
    }
}
