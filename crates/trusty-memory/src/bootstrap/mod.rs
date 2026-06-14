//! Knowledge-graph bootstrap helpers.
//!
//! Why: Issue #60 — after `palace_create`, the knowledge graph (KG) sits at
//! zero triples and there is no auto-discovery path. Users have no idea
//! they're supposed to call `kg_assert` manually before `kg_query` returns
//! anything useful. `kg_bootstrap` closes this gap by scanning well-known
//! project files (`Cargo.toml`, `package.json`, `pyproject.toml`, `CLAUDE.md`,
//! `.git/config`, `go.mod`) and seeding structured triples that describe the
//! project (language, version, source repo, etc.). It also seeds temporal
//! metadata (`created_at`, `bootstrapped_at`) so even an empty project at
//! least has *something* in the KG and a timestamp anchor for future queries.
//! What: `scan_project` (in `scan`) returns a flat list of triples; the public
//! async entry point `bootstrap_palace` resolves a palace handle, runs the
//! scanner, and asserts each tuple through the existing `KnowledgeGraph::assert`
//! path. Types and helpers live in `types`.
//! Test: Unit tests in `scan` pin each scanner against fixture directories;
//! `kg_bootstrap` is exercised end-to-end from the MCP tool surface in
//! `tools.rs`.

mod scan;
mod types;

pub use types::{
    result_to_json, BootstrapResult, BootstrapTriple, ScannedFile, KG_EMPTY_HINT,
    is_kg_empty_for_subject,
};
pub use scan::scan_project;

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use trusty_common::memory_core::store::kg::Triple;

use crate::AppState;

/// Run the bootstrap scan against a palace.
///
/// Why: Single async entry point that the MCP dispatcher (and the
/// auto-bootstrap hook in `palace_create`) calls. Encapsulates path
/// resolution, scanning, triple construction, and KG assertion.
/// What: Resolves `project_path` (caller-supplied), runs the blocking
/// scanner, seeds temporal metadata triples, and asserts every discovery
/// through `handle.kg.assert(...)`. Returns a summary of what was written.
/// Test: `bootstrap_palace_seeds_temporal_metadata_when_no_files`,
/// `bootstrap_palace_scans_cargo_toml`.
pub async fn bootstrap_palace(
    state: &AppState,
    palace_id: &str,
    project_path: Option<&Path>,
) -> Result<BootstrapResult> {
    let handle = state
        .registry
        .open_palace(
            &state.data_root,
            &trusty_common::memory_core::palace::PalaceId::new(palace_id),
        )
        .with_context(|| format!("open palace {palace_id}"))?;

    // Choose the scan root. When the caller did not supply a project path,
    // we still scan the palace's own data dir so `CLAUDE.md` or other
    // operator-placed files inside the palace are picked up.
    let scan_root: PathBuf = match project_path {
        Some(p) => p.to_path_buf(),
        None => handle
            .data_dir
            .clone()
            .unwrap_or_else(|| state.data_root.join(palace_id)),
    };
    let palace_id_owned = palace_id.to_string();

    let (triples, scanned_files, project_subject) =
        tokio::task::spawn_blocking(move || scan_project(&scan_root, &palace_id_owned))
            .await
            .context("join scan_project")??;

    // Seed temporal metadata (always present, even for empty projects).
    let now = chrono::Utc::now();
    let mut all = triples;
    all.push(BootstrapTriple {
        subject: project_subject.clone(),
        predicate: "bootstrapped_at".to_string(),
        object: now.to_rfc3339(),
        provenance: "bootstrap:temporal".to_string(),
    });
    // `created_at` is only inserted when the palace doesn't yet have one;
    // re-running bootstrap must not lie about when the palace first came
    // into being. The KG's temporal layer would close the prior interval
    // and the new interval would carry a misleading `valid_from`. Check
    // `query_active` before writing.
    let existing = handle
        .kg
        .query_active(&project_subject)
        .await
        .context("kg.query_active for created_at check")?;
    if !existing.iter().any(|t| t.predicate == "created_at") {
        all.push(BootstrapTriple {
            subject: project_subject.clone(),
            predicate: "created_at".to_string(),
            object: now.to_rfc3339(),
            provenance: "bootstrap:temporal".to_string(),
        });
    }

    let mut asserted = 0usize;
    for bt in &all {
        let triple = Triple {
            subject: bt.subject.clone(),
            predicate: bt.predicate.clone(),
            object: bt.object.clone(),
            valid_from: now,
            valid_to: None,
            confidence: 1.0,
            provenance: Some(bt.provenance.clone()),
        };
        handle
            .kg
            .assert(triple)
            .await
            .with_context(|| format!("kg.assert {} {}", bt.subject, bt.predicate))?;
        asserted += 1;
    }

    Ok(BootstrapResult {
        palace: palace_id.to_string(),
        project_subject,
        triples_asserted: asserted,
        scanned_files,
    })
}
