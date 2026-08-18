//! Shared fixture for the two #5036 palace-alias lane tests.
//!
//! Why: the read test and the write test need the identical split-brain shape
//! — a slug with no directory of its own pointing at a palace that has one —
//! but they cannot share a test BINARY. The write test seeds the process-wide
//! mock embedder, and `shared_embedder_initialized()` is monotonic, so a seed
//! anywhere in the process would move the read test off the embedder-warming
//! path it depends on. Two binaries, one fixture.
//! What: the lane's env gate plus an `Aliased` fixture that creates the
//! canonical palace, registers the alias, and asserts the redirect fires before
//! a test starts.
//!
//! #5329 deleted this fixture's daemon-binary discovery. It existed because the
//! supervisor located `trusty-bm25-daemon` as a sibling of `current_exe()`,
//! which for an integration test is the test binary under `target/*/deps/` — so
//! the fixture had to find the real build output, pin
//! `TRUSTY_BM25_DAEMON_BIN` at it, and panic when no daemon had been built.
//! There is no binary to find now, so arming the lane is one env var.
//! Test: used by `bm25_alias_recall.rs` and `bm25_alias_write.rs`.

use trusty_common::memory_core::palace::{Palace, PalaceId};
use trusty_common::palace_alias::PalaceAliasStore;
use trusty_memory::AppState;

/// Turn the lexical lane on.
///
/// Why: these tests are about a lane that fails quietly, so their bootstrap
/// must not itself become a silent skip.
/// What: sets `TRUSTY_BM25_DAEMON=1` — the gate that keeps its daemon-era name
/// for compatibility (#5329).
/// Test: this is the test bootstrap.
fn arm_lane() {
    // SAFETY: test-only env mutation. Every test in a given binary sets the
    // same value, so a concurrent sibling cannot observe a different lane state.
    unsafe {
        std::env::set_var("TRUSTY_BM25_DAEMON", "1");
    }
}

/// A canonical palace on disk plus an alias that redirects to it.
///
/// Why: getting the shape wrong in either direction — an alias directory that
/// exists, or a target that does not — silently disables the redirect, and the
/// test would then pass against the bug. The constructor asserts the redirect
/// rather than assuming it.
/// What: creates `<data_root>/<canonical>/palace.json` through the real
/// registry and registers `alias -> canonical` in `palace_aliases.json`.
/// Test: used by both alias-lane tests.
pub struct Aliased {
    _tmp: tempfile::TempDir,
    pub state: AppState,
    pub canonical: String,
    pub alias: String,
}

impl Aliased {
    pub fn new(tag: &str) -> Self {
        arm_lane();
        let tmp = tempfile::tempdir().expect("tempdir");
        let data_root = tmp.path().to_path_buf();
        // Short names are no longer a `sun_path` requirement (#5329 removed the
        // socket); the pid suffix still keeps the two fixtures' palace ids
        // distinct per process.
        let suffix = format!("{tag}{:x}", std::process::id() & 0xffff);
        let canonical = format!("c{suffix}");
        let alias = format!("a{suffix}");

        let state = AppState::new(data_root.clone()).with_bm25_lane_from_env();
        assert!(
            state.bm25_lane().is_some(),
            "the lexical lane must be armed, or this test proves nothing"
        );

        state
            .registry
            .create_palace(
                &data_root,
                Palace {
                    id: PalaceId::new(canonical.clone()),
                    name: canonical.clone(),
                    description: None,
                    created_at: chrono::Utc::now(),
                    data_dir: data_root.join(&canonical),
                },
            )
            .expect("create canonical palace");
        PalaceAliasStore::register_alias(&data_root, &alias, &canonical)
            .expect("register palace alias");

        // Preconditions. The alias owns no directory — that absence is exactly
        // what keeps `palace_ids_on_disk` from ever enumerating it — and the
        // registry redirects a lookup for it to the canonical palace.
        assert!(
            !data_root.join(&alias).join("palace.json").exists(),
            "the alias must have no palace directory of its own"
        );
        let resolved = state
            .registry
            .open_palace(&data_root, &PalaceId::new(alias.clone()))
            .expect("open through the alias");
        assert_eq!(
            resolved.id.as_str(),
            canonical,
            "the registry must resolve the alias to the canonical palace"
        );

        Self {
            _tmp: tmp,
            state,
            canonical,
            alias,
        }
    }

    /// Flush the lane's snapshots and stop its ticker.
    pub async fn shutdown(&self) {
        if let Some(lane) = self.state.bm25_lane() {
            lane.shutdown().await;
        }
    }
}
