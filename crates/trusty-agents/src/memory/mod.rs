//! Embedded storage layer for agent memory and code search.
//!
//! Why: Centralize persistent storage for memories and code embeddings so
//! agents have a uniform, embeddable backend (no external services). redb
//! holds payload JSON and id/label mappings; usearch holds HNSW vector
//! indexes. Segments namespace the two primary use cases (agent memory vs.
//! code index) so they can evolve independently without key collisions.
//! What: Exposes `MemoryStore` trait + `RedbUsearchStore` concrete impl with
//! `Segment` enum to select the namespace, and `MemoryResult` for hits.
//! Test: See unit tests in `redb_usearch.rs` — insert + search round-trip,
//! segment isolation, get-by-id, and persistence across store reopens.
//!
//! NOTE: this module is scaffolded ahead of its consumers (issue #36). The
//! `#![allow(dead_code)]` below keeps the build clean until PM/agents start
//! calling it; unit tests in `redb_usearch.rs` exercise the public surface.

#![allow(dead_code)]

pub mod code_store;
pub mod embed;
pub mod graph;
pub mod redb_recovery;
pub mod redb_usearch;
pub mod scope;
pub mod session_store;
pub mod store;
pub mod trusty_backed;
pub mod trusty_client;
pub mod user_store;

#[allow(unused_imports)]
pub use code_store::CodeStore;
#[allow(unused_imports)]
pub use embed::{Embedder, FastEmbedder};
#[allow(unused_imports)]
pub use graph::{AgentSession, MemoryGraph};
#[allow(unused_imports)]
pub use redb_usearch::RedbUsearchStore;
#[allow(unused_imports)]
pub use scope::{MemoryScope, MemoryScopeError};
#[allow(unused_imports)]
pub use session_store::{SessionMeta, SessionRegistry, SessionStore};
#[allow(unused_imports)]
pub use store::{MemoryResult, MemoryStore, Segment};
#[allow(unused_imports)]
pub use trusty_backed::TrustyBackedMemoryStore;
#[allow(unused_imports)]
pub use trusty_client::{MemoryBackend, TrustyMemoryClient};

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};

/// Environment variable controlling which `MemoryStore` backend
/// `open_memory_store` instantiates.
///
/// Why: Issue #52 wires `TrustyBackedMemoryStore` behind a config flag so we
/// can flip the production default without breaking existing installs. Until
/// trusty-agents grows a central memory config section, an env var is the
/// least-invasive switch and matches the pattern set by `TRUSTY_DEVICE`.
/// What: Accepted values are "redb" (default — `RedbUsearchStore`) and
/// "trusty" (`TrustyBackedMemoryStore`). Unknown values fall back to "redb"
/// with a warning so a typo can't take memory offline.
/// Test: `open_memory_store_defaults_to_redb` and
/// `open_memory_store_selects_trusty_via_env`.
/// New canonical name.
pub const MEMORY_BACKEND_ENV: &str = "TAGENT_MEMORY_BACKEND";
/// Deprecated legacy name (OPEN_MPM_* era, before #831 rename) — still honoured via env_compat fallback.
pub const MEMORY_BACKEND_ENV_DEPRECATED: &str = "OPEN_MPM_MEMORY_BACKEND";

/// Open the configured `MemoryStore` for `data_dir`.
///
/// Why: Centralises the backend selection logic so call sites (ctrl/mod.rs,
/// docs seeder, tests) don't each re-implement the env-var lookup. Issue #52
/// gates the new trusty-backed implementation behind
/// `TAGENT_MEMORY_BACKEND=trusty` while keeping `RedbUsearchStore` the
/// default — no behaviour change for existing installs.
/// What: Reads `TAGENT_MEMORY_BACKEND` (case-insensitive). On "trusty",
/// returns an `Arc<TrustyBackedMemoryStore>`; on any other value (including
/// unset), returns an `Arc<RedbUsearchStore>` opened with the standard
/// embedding dimension. The trusty branch stores its files under
/// `<data_dir>/trusty/` so a redb store at the same `data_dir` is untouched
/// and operators can flip back without manual cleanup.
/// Test: `open_memory_store_defaults_to_redb`,
/// `open_memory_store_selects_trusty_via_env`.
pub fn open_memory_store(data_dir: &Path) -> Result<Arc<dyn store::MemoryStore>> {
    open_memory_store_scoped(data_dir, None)
}

/// The configured `MemoryStore` for ONE assistant under `data_dir` (#7443).
///
/// Why: the native memory tools address `Segment::AgentMemory`, which an
/// unscoped store maps onto one process-global drawer — two assistants served
/// by one process would read and write each other's memories. This is the ONE
/// entry point that opens a store an assistant may be handed, and it takes no
/// argument that turns the scope back off.
/// What: [`open_memory_store`] with `scope` threaded through — a per-assistant
/// palace id AND a per-assistant data root, because the two backends isolate on
/// different things.
/// Test: `two_assistants_never_cross_read`,
/// `a_scoped_store_is_rooted_under_the_assistant`.
pub fn open_memory_store_for_assistant(
    data_dir: &Path,
    scope: &MemoryScope,
) -> Result<Arc<dyn store::MemoryStore>> {
    open_memory_store_scoped(data_dir, Some(scope))
}

/// Shared body of [`open_memory_store`] and [`open_memory_store_for_assistant`].
///
/// Why: one backend-selection match, so a backend added to one entry point
/// cannot be forgotten in the other.
/// Test: `open_memory_store_defaults_to_redb`,
/// `open_memory_store_selects_trusty_via_env`.
fn open_memory_store_scoped(
    data_dir: &Path,
    scope: Option<&MemoryScope>,
) -> Result<Arc<dyn store::MemoryStore>> {
    let backend = crate::env_compat::env_var(MEMORY_BACKEND_ENV, MEMORY_BACKEND_ENV_DEPRECATED)
        .unwrap_or_default();
    // #7443: redb isolates on its data dir, trusty on its palace id; scoping
    // both is what makes the guarantee independent of the selected backend.
    let redb_dir = match scope {
        Some(scope) => data_dir.join("assistants").join(scope.as_str()),
        None => data_dir.to_path_buf(),
    };
    let open_redb = |label: &str| -> Result<Arc<dyn store::MemoryStore>> {
        let s =
            RedbUsearchStore::open(&redb_dir, embed::ALL_MINI_LM_L6_V2_DIM).with_context(|| {
                format!("open RedbUsearchStore at {} ({label})", redb_dir.display())
            })?;
        Ok(Arc::new(s))
    };
    match backend.to_ascii_lowercase().as_str() {
        "trusty" => {
            let trusty_dir = data_dir.join("trusty");
            let s = match scope {
                Some(scope) => {
                    TrustyBackedMemoryStore::new_for_assistant(&trusty_dir, scope.clone())
                }
                None => TrustyBackedMemoryStore::new(&trusty_dir),
            }
            .with_context(|| {
                format!(
                    "open TrustyBackedMemoryStore at {} (backend=trusty)",
                    trusty_dir.display()
                )
            })?;
            tracing::info!(path = %trusty_dir.display(), "memory backend: trusty");
            Ok(Arc::new(s))
        }
        "" | "redb" => open_redb("backend=redb"),
        other => {
            tracing::warn!(
                requested = other,
                "unknown {MEMORY_BACKEND_ENV} value; falling back to redb"
            );
            open_redb("fallback")
        }
    }
}

/// Migrate an old-layout `.trusty-agents/` directory to the new split layout.
///
/// Why: Existing installations have `.trusty-agents/store/{store.redb,
/// code.usearch, mem.usearch}` — a monolithic store shared between code and
/// agent memory. Issue #45 splits these into `code/` and `sessions/<run_id>/`
/// so concurrent sessions don't clobber each other. We keep the
/// code-index redb + usearch files (moving them into `code/`) but start the
/// session-store redb fresh because a single redb file can't be cleanly
/// split between segments without a full rewrite.
/// What: If `.trusty-agents/store/` exists and `.trusty-agents/code/` does not, move
/// `store.redb` -> `code/store.redb` and `code.usearch` -> `code/code.usearch`.
/// Also moves `mem.usearch` (if any) into `sessions/default/mem.usearch`
/// so legacy agent-memory vector data is preserved; a fresh `store.redb` for
/// the session is created on next open. No-ops if the new layout already
/// exists.
/// Test: Covered by integration with manual run; unit testable via `migrate`
/// fixture in the future.
pub fn migrate_if_needed(agent_dir: &Path) -> Result<()> {
    let old_store = agent_dir.join("store");
    let code_dir = agent_dir.join("code");
    let sessions_default = agent_dir.join("sessions").join("default");

    if !old_store.exists() || code_dir.exists() {
        return Ok(());
    }

    std::fs::create_dir_all(&code_dir)
        .with_context(|| format!("creating {}", code_dir.display()))?;
    std::fs::create_dir_all(&sessions_default)
        .with_context(|| format!("creating {}", sessions_default.display()))?;

    let move_if_exists = |from: &Path, to: &Path| -> Result<()> {
        if from.exists() {
            std::fs::rename(from, to)
                .with_context(|| format!("moving {} -> {}", from.display(), to.display()))?;
        }
        Ok(())
    };

    move_if_exists(&old_store.join("store.redb"), &code_dir.join("store.redb"))?;
    move_if_exists(
        &old_store.join("code.usearch"),
        &code_dir.join("code.usearch"),
    )?;
    move_if_exists(
        &old_store.join("mem.usearch"),
        &sessions_default.join("mem.usearch"),
    )?;

    // Remove the now-empty old dir if nothing remains (best-effort).
    let _ = std::fs::remove_dir(&old_store);

    tracing::info!(
        from = %old_store.display(),
        code_dir = %code_dir.display(),
        sessions_default = %sessions_default.display(),
        "migrated legacy .trusty-agents/store layout"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    // Why: These tests hold `ENV_GUARD` (a `std::sync::Mutex`) across async
    // I/O to serialize global env-var mutation between tests. See the
    // ENV_GUARD doc comment below for the full rationale.
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use serde_json::json;
    use std::sync::Mutex;
    use tempfile::tempdir;

    /// Serializes the two tests that read/write the process-global
    /// `MEMORY_BACKEND_ENV` variable.
    ///
    /// Why: `std::env` is shared across every thread in the test binary, so two
    /// tests that mutate `TAGENT_MEMORY_BACKEND` can race when cargo runs them
    /// concurrently — one test's `set_var("trusty")` could still be live when
    /// the other reads it, flipping the default-backend test onto the trusty
    /// branch (the original flake at `mod.rs:210`). Holding this mutex for the
    /// whole env-dependent window guarantees the env var is in a known state for
    /// exactly one test at a time.
    /// What: a `std::sync::Mutex<()>` guard acquired at the top of each
    /// env-mutating test and held until the assertion completes.
    /// Test: `open_memory_store_defaults_to_redb` /
    /// `open_memory_store_selects_trusty_via_env` no longer flake under
    /// concurrent execution.
    static ENV_GUARD: Mutex<()> = Mutex::new(());

    /// Why: Default behaviour must remain `RedbUsearchStore` so unset
    /// environments behave identically before and after issue #52.
    /// What: Clear the env var, open the store, write+read a payload, and
    /// confirm the redb store path was created (proxy for "redb backend was
    /// selected").
    /// Test: This test itself.
    #[tokio::test]
    async fn open_memory_store_defaults_to_redb() {
        // Hold the env guard for the whole window so the trusty test's
        // `set_var` cannot leak into our `open_memory_store` read. Recover the
        // lock on poison — a panicking sibling test must not wedge this one.
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var(MEMORY_BACKEND_ENV).ok();
        // SAFETY: serialized by ENV_GUARD; restored before return.
        unsafe {
            std::env::remove_var(MEMORY_BACKEND_ENV);
        }
        let dir = tempdir().unwrap();
        let store = open_memory_store(dir.path()).expect("open default backend");
        store
            .insert(
                store::Segment::AgentMemory,
                "k",
                &vec![0.0_f32; embed::ALL_MINI_LM_L6_V2_DIM],
                json!({"ok": true}),
            )
            .await
            .expect("insert into default backend");
        // The redb store creates a store.redb file directly under data_dir.
        assert!(
            dir.path().join("store.redb").exists(),
            "redb backend should create store.redb under data_dir"
        );
        // SAFETY: serialized by ENV_GUARD.
        unsafe {
            match prior {
                Some(v) => std::env::set_var(MEMORY_BACKEND_ENV, v),
                None => std::env::remove_var(MEMORY_BACKEND_ENV),
            }
        }
    }

    /// Why: Setting `TAGENT_MEMORY_BACKEND=trusty` must select the
    /// trusty-backed adapter — this is the issue-#52 production flag.
    /// What: Set the env var, open the store, write a payload, and confirm
    /// the trusty palace layout (a `trusty/` subdir + `payloads.db`) exists.
    /// Test: This test itself.
    #[tokio::test]
    async fn open_memory_store_selects_trusty_via_env() {
        // Serialize against the default-backend test (see ENV_GUARD).
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var(MEMORY_BACKEND_ENV).ok();
        let dir = tempdir().unwrap();
        // SAFETY: serialized by ENV_GUARD; restored before return.
        unsafe {
            std::env::set_var(MEMORY_BACKEND_ENV, "trusty");
        }
        let result = open_memory_store(dir.path());
        // SAFETY: serialized by ENV_GUARD.
        unsafe {
            match prior {
                Some(v) => std::env::set_var(MEMORY_BACKEND_ENV, v),
                None => std::env::remove_var(MEMORY_BACKEND_ENV),
            }
        }
        let store = result.expect("open trusty backend");
        store
            .insert(
                store::Segment::AgentMemory,
                "k",
                &vec![0.0_f32; embed::ALL_MINI_LM_L6_V2_DIM],
                json!({"ok": true}),
            )
            .await
            .expect("insert into trusty backend");
        assert!(
            dir.path().join("trusty").join("payloads.redb").exists(),
            "trusty backend should create payloads.redb under <data_dir>/trusty/"
        );
    }

    /// #7443: two assistants sharing a data dir never read each other's rows,
    /// whichever backend is configured.
    ///
    /// Why: redb isolates on its data dir and the trusty adapter on its palace
    /// id, so a fix that only scoped one of them would hold under one backend
    /// and leak under the other. This test reads no env var and sets none, so
    /// it exercises whichever backend the runner has configured.
    /// What: two scoped stores under one data dir, the same key written through
    /// each, read back through each.
    /// Test: this test itself; the tool-level proof is
    /// `crate::tools::native_memory::tests::two_assistants_never_cross_read`.
    #[tokio::test]
    async fn a_scoped_store_is_rooted_under_the_assistant() {
        let dir = tempdir().unwrap();
        let alpha = MemoryScope::new("alpha").expect("alpha");
        let beta = MemoryScope::new("beta").expect("beta");
        let a = open_memory_store_for_assistant(dir.path(), &alpha).expect("alpha store");
        let b = open_memory_store_for_assistant(dir.path(), &beta).expect("beta store");
        let vec = vec![0.0_f32; embed::ALL_MINI_LM_L6_V2_DIM];
        a.insert(
            store::Segment::AgentMemory,
            "k",
            &vec,
            json!({"who": "alpha"}),
        )
        .await
        .expect("alpha insert");
        b.insert(
            store::Segment::AgentMemory,
            "k",
            &vec,
            json!({"who": "beta"}),
        )
        .await
        .expect("beta insert");
        assert_eq!(
            a.get(store::Segment::AgentMemory, "k").await.unwrap(),
            Some(json!({"who": "alpha"})),
            "alpha read beta's row"
        );
        assert_eq!(
            b.get(store::Segment::AgentMemory, "k").await.unwrap(),
            Some(json!({"who": "beta"})),
            "beta read alpha's row"
        );
    }
}
