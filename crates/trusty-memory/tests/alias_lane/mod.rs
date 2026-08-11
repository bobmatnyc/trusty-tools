//! Shared fixture for the two #5036 palace-alias lane tests.
//!
//! Why: the read test and the write test need the identical split-brain shape
//! — a slug with no directory of its own pointing at a palace that has one —
//! but they cannot share a test BINARY. The write test seeds the process-wide
//! mock embedder, and `shared_embedder_initialized()` is monotonic, so a seed
//! anywhere in the process would move the read test off the embedder-warming
//! path it depends on. Two binaries, one fixture.
//! What: daemon-binary discovery, the lane's env gate, and an `Aliased`
//! fixture that creates the canonical palace, registers the alias, and asserts
//! the redirect fires before a test starts.
//! Test: used by `bm25_alias_recall.rs` and `bm25_alias_write.rs`.

use std::path::PathBuf;

use trusty_common::memory_core::palace::{Palace, PalaceId};
use trusty_common::palace_alias::PalaceAliasStore;
use trusty_memory::AppState;

/// Resolve the freshly-built `trusty-bm25-daemon` binary.
///
/// Why: the supervisor discovers the daemon as a sibling of `current_exe()`,
/// which for an integration test is the test binary under `target/*/deps/`.
/// Pointing `TRUSTY_BM25_DAEMON_BIN` at the real build output sidesteps that.
/// What: honours the env var if already set, else walks up from the test
/// binary looking for the daemon next to it or one level up.
/// Test: this is the test bootstrap.
fn discover_daemon_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("TRUSTY_BM25_DAEMON_BIN") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let mut p = exe.as_path();
    while let Some(parent) = p.parent() {
        let candidate = parent.join("trusty-bm25-daemon");
        if candidate.is_file() {
            return Some(candidate);
        }
        let candidate = parent.join("..").join("trusty-bm25-daemon");
        if candidate.is_file() {
            return Some(candidate);
        }
        p = parent;
    }
    None
}

/// Turn the lane on and pin the supervisor's locator at the built daemon.
///
/// Why: a missing binary used to be a silent skip, which reads as a pass. These
/// tests are about a lane that fails quietly, so their bootstrap must not.
/// What: panics with the build command when the daemon is absent; otherwise
/// sets `TRUSTY_BM25_DAEMON=1`, pins `TRUSTY_BM25_DAEMON_BIN`, and clears
/// `TRUSTY_BM25_EXTERNAL` so the real supervisor runs.
/// Test: this is the test bootstrap.
fn arm_lane() {
    let binary = discover_daemon_binary().expect(
        "trusty-bm25-daemon binary not found — build it first \
         (`cargo build -p trusty-memory --bin trusty-bm25-daemon`) or set \
         TRUSTY_BM25_DAEMON_BIN=<path>",
    );
    // SAFETY: test-only env mutation. Every test in a given binary sets the
    // same three values, so a concurrent sibling cannot observe a different
    // lane state.
    unsafe {
        std::env::set_var("TRUSTY_BM25_DAEMON_BIN", &binary);
        std::env::set_var("TRUSTY_BM25_DAEMON", "1");
        std::env::remove_var("TRUSTY_BM25_EXTERNAL");
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
        // Short names: the socket path must stay inside `sun_path` (~104
        // bytes) and macOS `$TMPDIR` already spends about half of it.
        let suffix = format!("{tag}{:x}", std::process::id() & 0xffff);
        let canonical = format!("c{suffix}");
        let alias = format!("a{suffix}");

        let state = AppState::new(data_root.clone()).with_bm25_client_from_env();
        assert!(
            state.bm25_client.is_some() && state.bm25_supervisor.is_some(),
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

    /// Reap the daemons this fixture's supervisor started.
    pub async fn shutdown(&self) {
        if let Some(sup) = self.state.bm25_supervisor.as_ref() {
            sup.shutdown().await;
        }
    }
}
