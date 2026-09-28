//! #8733: two would-be maintainers on one data root run one dream pass.
//!
//! Each registry carries its own `MaintenanceLease`, so each holds its own
//! lock descriptor; `flock` conflicts per open file description, which makes
//! the two registries compete exactly as two processes would.
//! Test: itself.

use super::config::{DreamConfig, PersistedDreamStats};
use super::dreamer::Dreamer;
use crate::memory_core::PalaceRegistry;
use crate::memory_core::maintenance_lease::MaintenanceLease;
use crate::memory_core::palace::{Palace, PalaceId};
use crate::memory_core::retrieval::{PalaceHandle, seed_shared_embedder_with_mock};
use crate::memory_core::semantic_consolidation::SemanticConsolidationConfig;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Open `name` under `root` into a registry gated on its own lease.
fn maintainer(root: &Path, name: &str) -> (PalaceRegistry, PalaceId, PathBuf) {
    seed_shared_embedder_with_mock();
    let palace = Palace {
        id: PalaceId::new(name),
        name: name.into(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: root.join(name),
    };
    std::fs::create_dir_all(&palace.data_dir).expect("palace dir");
    let registry =
        PalaceRegistry::new().with_maintenance_lease(Arc::new(MaintenanceLease::new(root)));
    registry.register_arc(PalaceHandle::open(&palace).expect("open palace"));
    (registry, palace.id, palace.data_dir)
}

fn dreamed(data_dir: &Path) -> bool {
    PersistedDreamStats::load(data_dir)
        .expect("load dream stats")
        .is_some()
}

/// Poll until `data_dir` records a dream pass or `within` elapses.
async fn wait_dreamed(data_dir: &Path, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if dreamed(data_dir) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    dreamed(data_dir)
}

/// Why: pre-#8733 every process with a dream loop dreamed, so two daemons on
/// one root both ran dedup. What: two loops with `idle_secs = 1` on one root;
/// after the first pass lands, three more ticks pass and the other palace
/// must still have no pass. Then the holder goes away (its loop stops and its
/// lease drops, as a crash would release it) and the survivor takes over.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_maintainers_on_one_root_run_one_dream_pass() {
    let root = tempfile::tempdir().expect("tempdir");
    let config = DreamConfig {
        idle_secs: 1,
        semantic: SemanticConsolidationConfig {
            enabled: false,
            ..Default::default()
        },
        recall_benchmark_enabled: false,
        compact: false,
        ..DreamConfig::default()
    };
    let (reg_a, id_a, dir_a) = maintainer(root.path(), "palace-a");
    let (reg_b, id_b, dir_b) = maintainer(root.path(), "palace-b");
    let (tx_a, rx_a) = tokio::sync::watch::channel(false);
    let (tx_b, rx_b) = tokio::sync::watch::channel(false);
    let join_a = Arc::new(Dreamer::new(config.clone())).start_with_shutdown(
        reg_a.clone(),
        id_a,
        Duration::ZERO,
        rx_a,
    );
    let join_b = Arc::new(Dreamer::new(config)).start_with_shutdown(
        reg_b.clone(),
        id_b,
        Duration::ZERO,
        rx_b,
    );

    let first = tokio::select! {
        true = wait_dreamed(&dir_a, Duration::from_secs(15)) => 'a',
        true = wait_dreamed(&dir_b, Duration::from_secs(15)) => 'b',
        else => panic!("no dream pass ran within 15s"),
    };
    tokio::time::sleep(Duration::from_millis(3_200)).await;
    let (winner, loser_dir) = if first == 'a' {
        ((reg_a, tx_a, join_a), (dir_b.clone(), tx_b, join_b))
    } else {
        ((reg_b, tx_b, join_b), (dir_a.clone(), tx_a, join_a))
    };
    assert!(
        !dreamed(&loser_dir.0),
        "only the lease holder may dream; palace {} also ran a pass",
        loser_dir.0.display()
    );

    // The holder goes away: stop its loop and drop the last registry clone.
    let (winner_reg, winner_tx, winner_join) = winner;
    let _ = winner_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(5), winner_join).await;
    drop(winner_reg);
    assert!(
        wait_dreamed(&loser_dir.0, Duration::from_secs(10)).await,
        "the survivor must take over maintenance once the holder is gone"
    );
    let _ = loser_dir.1.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(5), loser_dir.2).await;
}
