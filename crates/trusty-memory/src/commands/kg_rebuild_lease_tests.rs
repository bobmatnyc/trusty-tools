//! Maintenance-lease gating of the destructive `kg-rebuild` passes (#8744).
//!
//! Why: `--purge-stale-subjects` deletes subjects and `--merge-punctuated-twins`
//! retracts rows. Both are maintenance, so while another process (the daemon,
//! mid-dream-pass) holds the data root's lease they must refuse and change
//! nothing. The holder here is a second `MaintenanceLease` in this process: a
//! `flock` conflicts per open file description, so it stands in for the daemon.
//! What: a held lease refuses the purge, the merge, and the whole command
//! before its rebuild step; an unopenable lease file refuses both passes.
//! Test: this module.

use std::sync::Arc;

use anyhow::Result;
use serde_json::json;
use trusty_common::memory_core::palace::PalaceId;
use trusty_common::memory_core::store::kg::{KnowledgeGraph, Triple};
use trusty_common::memory_core::MaintenanceLease;

use super::{kg_rebuild_at, purge_palaces, scan_active_triples, KgRebuildOptions};
use crate::commands::kg_twin_merge::merge_palaces;
use crate::kg_extract::AUTO_PROVENANCE;
use crate::AppState;

/// An active auto-extracted triple.
fn auto_triple(subject: &str, predicate: &str, object: &str) -> Triple {
    Triple {
        subject: subject.to_string(),
        predicate: predicate.to_string(),
        object: object.to_string(),
        valid_from: chrono::Utc::now(),
        valid_to: None,
        confidence: 0.6,
        provenance: Some(AUTO_PROVENANCE.to_string()),
    }
}

/// A ready `AppState` on `root` holding palace `a`, with the palace's KG.
async fn palace_a(root: &std::path::Path) -> Result<(AppState, Arc<KnowledgeGraph>)> {
    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();
    // Issue #88: bypass palace-slug enforcement for test palaces. #5937: the
    // write happens under the crate's env lock.
    {
        let _env = crate::commands::env_test_lock().lock().await;
        // SAFETY: idempotent constant write "1", made under the env lock.
        unsafe {
            std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
        }
    }
    let state = AppState::new(root.to_path_buf());
    state.set_ready();
    let _ = crate::tools::dispatch_tool(&state, "palace_create", json!({"name": "a"})).await?;
    let handle = state
        .registry
        .open_palace(&state.data_root, &PalaceId::new("a"))?;
    let kg = handle.kg.clone();
    Ok((state, kg))
}

/// The active `(subject, predicate, object)` rows of `kg`, sorted.
async fn active_rows(kg: &KnowledgeGraph) -> Result<Vec<(String, String, String)>> {
    let mut rows: Vec<_> = scan_active_triples(kg, "a")
        .await?
        .into_iter()
        .map(|t| (t.subject, t.predicate, t.object))
        .collect();
    rows.sort();
    Ok(rows)
}

/// Why: #8744 — the purge deleted subjects while the daemon held the lease.
/// What: with the lease held elsewhere an applying purge fails naming the
/// holder's pid and the stale subject survives. Once the holder is gone the
/// same purge runs, and afterwards the lease is free again, so the pass
/// releases it on return. Pre-fix the first purge deleted `them`.
/// Test: This test.
#[tokio::test]
async fn purge_under_a_lease_held_elsewhere_refuses_and_deletes_nothing() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let (state, kg) = palace_a(tmp.path()).await?;
    kg.assert(auto_triple("them", "is-a", "thing")).await?;
    let before = active_rows(&kg).await?;

    let holder = MaintenanceLease::new(tmp.path());
    assert!(holder.try_hold().is_held());
    let err = purge_palaces(&state, Some("a"), true)
        .await
        .expect_err("an applying purge must refuse while another process holds the lease");
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains(&format!("pid {}", std::process::id())),
        "the refusal must name the holder's pid, got {rendered}"
    );
    assert_eq!(
        active_rows(&kg).await?,
        before,
        "a refused purge changed the KG"
    );

    drop(holder);
    let applied = purge_palaces(&state, Some("a"), true).await?;
    assert_eq!(applied[0].deleted, vec!["them".to_string()]);
    assert!(
        MaintenanceLease::new(tmp.path()).try_hold().is_held(),
        "the purge must release the lease when it returns"
    );
    Ok(())
}

/// Why: #8744 — the twin merge retracted rows while the daemon held the lease.
/// What: with the lease held elsewhere an applying merge fails naming the
/// holder's pid and neither node moves. Pre-fix it re-pointed the twin.
/// Test: This test.
#[tokio::test]
async fn merge_under_a_lease_held_elsewhere_refuses_and_repoints_nothing() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let (state, kg) = palace_a(tmp.path()).await?;
    kg.assert(auto_triple("`redb`", "uses", "mmap")).await?;
    let before = active_rows(&kg).await?;

    let holder = MaintenanceLease::new(tmp.path());
    assert!(holder.try_hold().is_held());
    let err = merge_palaces(&state, Some("a"), true)
        .await
        .expect_err("an applying merge must refuse while another process holds the lease");
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains(&format!("pid {}", std::process::id())),
        "the refusal must name the holder's pid, got {rendered}"
    );
    assert_eq!(
        active_rows(&kg).await?,
        before,
        "a refused merge changed the KG"
    );
    Ok(())
}

/// Why: #8744 requires the lease check to run before the rebuild step of
/// `handle_kg_rebuild_with`, which had no test. A check placed after it would
/// leave the rebuild's asserts landing under a live holder.
/// What: clears the auto triples a remembered drawer produced, seeds a stale
/// subject, then runs the command with `--purge-stale-subjects` under a held
/// lease. It must fail naming the holder and leave the active set exactly as
/// seeded: the rebuild re-asserts nothing and the purge deletes nothing.
/// Pre-fix the rebuild restored the drawer's triples and the purge removed
/// `them`.
/// Test: This test.
#[tokio::test]
async fn kg_rebuild_refuses_before_the_rebuild_while_the_lease_is_held() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let (state, kg) = palace_a(tmp.path()).await?;
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
    let store = kg.store();
    for (subject, _, _) in active_rows(&kg).await? {
        store.delete_by_subject(&subject)?;
    }
    kg.assert(auto_triple("them", "is-a", "thing")).await?;
    let seeded = active_rows(&kg).await?;
    assert_eq!(seeded.len(), 1, "fixture: only the stale row is active");

    let holder = MaintenanceLease::new(tmp.path());
    assert!(holder.try_hold().is_held());
    let opts = KgRebuildOptions {
        palace: Some("a".to_string()),
        purge_stale_subjects: true,
        ..KgRebuildOptions::default()
    };
    let err = kg_rebuild_at(tmp.path().to_path_buf(), opts)
        .await
        .expect_err("kg-rebuild --purge-stale-subjects must refuse under a held lease");
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains(&format!("pid {}", std::process::id())),
        "the refusal must name the holder's pid, got {rendered}"
    );
    assert_eq!(
        active_rows(&kg).await?,
        seeded,
        "a refused kg-rebuild wrote"
    );
    Ok(())
}

/// Why: #8744 — the lease must fail closed. A lease file that cannot be opened
/// is not permission to run maintenance.
/// What: `maintenance.lock` is a directory, so opening it for writing fails
/// and the lease reports `Unavailable`. Both applying passes must refuse with
/// that reason and leave the KG as seeded. Pre-fix both ran.
/// Test: This test.
#[tokio::test]
async fn destructive_passes_fail_closed_on_an_unavailable_lease() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let (state, kg) = palace_a(tmp.path()).await?;
    kg.assert(auto_triple("them", "is-a", "thing")).await?;
    kg.assert(auto_triple("`redb`", "uses", "mmap")).await?;
    let before = active_rows(&kg).await?;
    std::fs::create_dir(
        tmp.path()
            .join(trusty_common::memory_core::maintenance_lease::MAINTENANCE_LOCK_FILE),
    )?;

    let purge = purge_palaces(&state, Some("a"), true)
        .await
        .expect_err("the purge must refuse without a usable lease");
    let merge = merge_palaces(&state, Some("a"), true)
        .await
        .expect_err("the merge must refuse without a usable lease");
    for err in [purge, merge] {
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("unavailable"),
            "the refusal must say the lease is unavailable, got {rendered}"
        );
    }
    assert_eq!(
        active_rows(&kg).await?,
        before,
        "a refused pass changed the KG"
    );
    Ok(())
}
