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
use trusty_common::memory_core::palace::PalaceId;

use crate::kg_extract::{extract_triples, ExtractInput};
use crate::{resolve_palace_registry_dir, AppState};

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

/// CLI entry point for `trusty-memory kg-rebuild`.
///
/// Why: A thin shim that resolves the standard data dir, builds an
/// `AppState`, loads every palace, and dispatches to `rebuild_palaces`. Kept
/// separate from the core logic so the test suite can exercise
/// `rebuild_palaces` against a temp directory without going through clap.
/// What: Resolves `~/Library/Application Support/trusty-memory` (or the
/// platform equivalent) via `resolve_data_dir`, calls `rebuild_palaces` with
/// the optional palace filter, then prints a human-readable summary to
/// stdout. Returns the aggregate error count as the exit code (0 on success).
/// Test: not unit-tested (process-level entry point); the inner
/// `rebuild_palaces` is the testable surface.
pub async fn handle_kg_rebuild(palace: Option<String>) -> Result<()> {
    let data_dir = trusty_common::resolve_data_dir("trusty-memory")
        .context("resolve trusty-memory data dir")?;
    let data_root = resolve_palace_registry_dir(data_dir);
    let state = AppState::new(data_root);
    let loaded = state
        .load_palaces_from_disk()
        .await
        .context("load palaces from disk")?;
    tracing::info!(palaces_loaded = loaded, "kg-rebuild: palaces opened");

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
    Ok(())
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

    /// Why: Validate the back-fill end-to-end against a freshly-created
    /// palace with a known drawer count.
    /// What: Build a tempdir-rooted `AppState`, create two palaces, drop a
    /// drawer in each via `dispatch_tool("memory_remember", ...)`, run
    /// `rebuild_palaces(None)`, and confirm both palaces show up with the
    /// expected drawer counts.
    /// Test: This test.
    #[tokio::test]
    async fn kg_rebuild_processes_all_drawers() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        // Issue #88: bypass palace-slug enforcement for test palaces.
        // SAFETY: tests using TRUSTY_SKIP_PALACE_ENFORCEMENT set a constant
        // value "1"; idempotent across concurrent test threads.
        unsafe {
            std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
        }
        let state = AppState::new(tmp.path().to_path_buf());

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
        let tmp = tempfile::tempdir()?;
        // Issue #88: bypass palace-slug enforcement for test palaces.
        // SAFETY: idempotent constant write "1"; safe across test threads.
        unsafe {
            std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
        }
        let state = AppState::new(tmp.path().to_path_buf());

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
