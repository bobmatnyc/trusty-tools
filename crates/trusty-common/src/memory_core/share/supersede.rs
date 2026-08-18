//! Supersession: the ONE `superseded_by` writer, shared by the dream cycle and
//! the share path (#5902).
//!
//! Why: an edited memory has different content, so it has a different content
//! hash, so it is a different memory. The old one must not simply orphan — a
//! reader that finds only the new fact has no way back to what it replaced, and
//! the estate hand-wrote 109 amendment edges precisely so corrections stay
//! traceable (ADR-0028 D6). The mechanism for that already existed:
//! `dream::cycle::record_provenance_and_collect_superseded` asserts
//! `Triple { subject: "drawer:{orig}", predicate: "superseded_by",
//! object: "drawer:{canonical}" }` and — this is the load-bearing half, issue
//! #1713 — only reports the original as evictable once that triple write
//! durably succeeded. Before #1713, consolidation pushed every original onto the
//! eviction list whether or not the provenance landed, so a canonical drawer
//! could exist with the original gone and no link back.
//!
//! Rather than write a second supersede concept for the share path, this module
//! IS that writer and the dream cycle now calls it. CLAUDE.md's
//! common-entry-point rule: the guarantee lands once, so a fix to it cannot land
//! in one copy and miss the other.
//!
//! What: [`assert_superseded_by`] (one edge, fail-loud) and
//! [`supersede_drawer`] (write the replacement, then link it, and report whether
//! the original may be retired).
//! Test: `supersede_mints_a_new_hash_and_links_the_original`,
//! `assert_superseded_by_fails_loud_on_an_unwritable_kg`,
//! `dream::tests::apply_consolidation_result_keeps_original_when_kg_write_fails`.

use anyhow::{Context, Result};
use uuid::Uuid;

use crate::memory_core::palace::RoomType;
use crate::memory_core::retrieval::{PalaceHandle, RememberOptions};
use crate::memory_core::store::kg::{KnowledgeGraph, Triple};

/// The predicate every supersession edge uses.
///
/// Why: two spellings of this string would split the amendment graph in half
/// while every write still reported success. It is read by KG queries in
/// `trusty-memory`, so it is a wire constant, not an implementation detail.
pub const SUPERSEDED_BY: &str = "superseded_by";

/// Assert that `original` was superseded by `replacement`.
///
/// Why: the caller's next step is almost always to retire, evict, or stop
/// surfacing `original`, and that step is only safe once this edge is durable.
/// So this returns `Result` and never swallows the failure — a caller that
/// treated a failed provenance write as success is exactly the #1713 defect.
/// What: asserts the `drawer:{original}` → `superseded_by` → `drawer:{replacement}`
/// triple with confidence 1.0 and `provenance` naming the pass that decided it.
/// Test: `supersede_mints_a_new_hash_and_links_the_original`,
/// `assert_superseded_by_fails_loud_on_an_unwritable_kg`.
pub async fn assert_superseded_by(
    kg: &KnowledgeGraph,
    original: Uuid,
    replacement: Uuid,
    provenance: &str,
) -> Result<()> {
    let triple = Triple {
        subject: format!("drawer:{original}"),
        predicate: SUPERSEDED_BY.to_string(),
        object: format!("drawer:{replacement}"),
        valid_from: chrono::Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: Some(provenance.to_string()),
    };
    kg.assert(triple)
        .await
        .with_context(|| format!("assert superseded_by for drawer {original}"))
}

/// What a supersession left behind.
///
/// Test: `supersede_mints_a_new_hash_and_links_the_original`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Supersession {
    /// The id of the newly written replacement drawer.
    pub replacement: Uuid,
    /// Whether the `superseded_by` edge landed durably. ONLY when this is `true`
    /// may a caller retire, evict, or stop surfacing the original — issue #1713.
    pub linked: bool,
}

/// Replace a memory's body with `new_content`, minting a new identity and
/// linking the old one to it (#5902).
///
/// Why: this is what "editing a memory" means once identity is content-derived.
/// The body changes, so the content hash changes, so what exists afterwards is a
/// NEW memory — there is no in-place edit that preserves identity, and pretending
/// otherwise is what would strand the original. Writing the replacement first and
/// linking second is deliberate: if the link fails, the estate holds two live
/// drawers and a missing edge, which a re-run can repair. Reversed, a failed
/// write after a successful link would leave an edge pointing at nothing.
///
/// What: writes `new_content` through `PalaceHandle::remember_with_options` — so
/// it passes the same filter, classification, and Tier C gates as any other write
/// — then asserts the supersession edge. `linked: false` means the caller must
/// leave the original entirely alone. This function never evicts, forgets, or
/// tombstones anything: supersession is an added edge, and demotion is the
/// caller's decision (ADR-0028 D6, "demoted, never deleted").
/// Test: `supersede_mints_a_new_hash_and_links_the_original`,
/// `assert_superseded_by_fails_loud_on_an_unwritable_kg`.
pub async fn supersede_drawer(
    handle: &PalaceHandle,
    original: Uuid,
    new_content: impl Into<String>,
    room: RoomType,
    tags: Vec<String>,
    importance: f32,
) -> Result<Supersession> {
    let replacement = handle
        .remember_with_options(
            new_content.into(),
            room,
            tags,
            importance,
            RememberOptions::forced(),
        )
        .await
        .context("write the superseding drawer")?;

    let linked =
        match assert_superseded_by(&handle.kg, original, replacement, "share:supersede").await {
            Ok(()) => true,
            Err(e) => {
                // #1713: the replacement is durable and the link is not, so the
                // original stays live and recoverable. Reporting `linked: false`
                // rather than erroring keeps the replacement's id in the caller's
                // hands — it exists, and losing track of it would be worse.
                tracing::warn!(
                    palace = %handle.id,
                    original = %original,
                    replacement = %replacement,
                    "#5902: superseded_by write failed; the original must NOT be \
                     retired: {e:#}"
                );
                false
            }
        };

    Ok(Supersession {
        replacement,
        linked,
    })
}
