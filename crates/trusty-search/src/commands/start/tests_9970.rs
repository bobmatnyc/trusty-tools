//! #9970: a search answered while the python embedder installs tells the
//! agent so, and one answered after the swap does not.
//!
//! Why: owner ruling D1b — the ~3 GB torch download runs in the background
//! and the daemon's response to the agent carries a note while it runs; Q5 —
//! an install that cannot finish leaves ort serving and says so.
//! What: a fake `PythonBootstrap` whose `ensure_venv` blocks until the test
//! releases it (or fails), driven by the real `drive_bootstrap`, while a
//! search goes through the daemon's own `search.query` socket method. Both
//! tests use only pre-#9970 APIs, so they compile on the base commit and fail
//! there on the missing `meta.embedder`.
//! Test: the tests below.

use std::sync::atomic::AtomicU32;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use trusty_common::embedder::MockEmbedder;
use trusty_common::uds::server::RpcRouter;

use super::graceful_bootstrap::{
    drive_bootstrap, PythonAdapterTeardown, PythonBootstrap, VenvCheck,
};
use crate::core::Embedder;
use crate::service::embedder_supervisor::{
    ActiveBackend, BackendKind, BootstrapState, SwitchableEmbedder,
};
use crate::service::SearchAppState;

const INDEX: &str = "note-9970";
const DIM: usize = trusty_common::embedder::EMBED_DIM;

/// How the fake's `ensure_venv` behaves.
enum Venv {
    /// Signal `entered`, then wait for the test to release it.
    Block {
        entered: Arc<tokio::sync::Notify>,
        release: Mutex<mpsc::Receiver<()>>,
    },
    /// Fail the way `TRUSTY_UV_FETCH=0` with no uv does.
    Fail,
}

struct FakeBootstrap(Venv);

struct NoTeardown;

#[async_trait::async_trait]
impl PythonAdapterTeardown for NoTeardown {
    async fn teardown(&self) {}
    async fn is_confirmed_terminated(&self) -> bool {
        false
    }
}

impl PythonBootstrap for FakeBootstrap {
    fn ensure_venv(&self, _check: VenvCheck) -> anyhow::Result<()> {
        match &self.0 {
            Venv::Block { entered, release } => {
                entered.notify_one();
                let _ = release.lock().expect("release lock").recv();
                Ok(())
            }
            Venv::Fail => anyhow::bail!("uv not found and TRUSTY_UV_FETCH=0 turns off fetching"),
        }
    }

    fn locate_launcher(&self) -> anyhow::Result<std::path::PathBuf> {
        Ok(std::path::PathBuf::from("/fake/trusty-embedderd-py"))
    }

    fn build_adapter(
        &self,
        _launcher: std::path::PathBuf,
        _budget: Duration,
    ) -> (
        Arc<dyn Embedder>,
        Option<Arc<AtomicU32>>,
        Arc<dyn PythonAdapterTeardown>,
    ) {
        (Arc::new(MockEmbedder::new(DIM)), None, Arc::new(NoTeardown))
    }

    fn probe_timeout(&self) -> Duration {
        Duration::from_secs(30)
    }
}

/// A daemon state with one empty, lexically ready index and an ort
/// switchable embedder in `Bootstrapping`, as the graceful default starts.
async fn state_on_ort(dir: &std::path::Path) -> (SearchAppState, Arc<SwitchableEmbedder>) {
    use crate::core::indexer::CodeIndexer;
    use crate::core::registry::{IndexHandle, IndexId, IndexRegistry, StageStatus};
    use crate::core::store::{UsearchStore, VectorStore};

    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(DIM));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(DIM).expect("usearch"));
    let indexer = CodeIndexer::new(INDEX, dir).with_components(Arc::clone(&embedder), store);
    let registry = IndexRegistry::new();
    let handle = IndexHandle::bare(
        IndexId::new(INDEX),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        dir.to_path_buf(),
    );
    let stages = Arc::clone(&handle.stages);
    registry.register(handle);
    stages.write().await.lexical.status = StageStatus::Ready;
    let switchable = Arc::new(SwitchableEmbedder::new(
        Arc::clone(&embedder),
        ActiveBackend {
            kind: BackendKind::Ort,
            provider: trusty_common::embedder::ExecutionProvider::Cpu,
            model: "all-MiniLM-L6-v2".to_string(),
            quantized: false,
            bootstrap: BootstrapState::Bootstrapping,
        },
    ));
    let state = SearchAppState::new(registry);
    state.install_embedder(embedder).await;
    state.install_switchable_embedder(Arc::clone(&switchable));
    (state, switchable)
}

/// `meta.embedder` of one `search.query` call through the socket router.
async fn search_embedder(state: &SearchAppState) -> Value {
    let state = Arc::new(state.clone());
    let router = crate::service::rpc::queries::register(RpcRouter::new(), &state);
    let frame = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "search.query",
        "params": { "index_id": INDEX, "body": { "text": "anything" } },
    });
    let resp = router.dispatch(frame.to_string().as_bytes()).await;
    let result = resp
        .result
        .unwrap_or_else(|| panic!("search failed: {:?}", resp.error));
    result["meta"]["embedder"].clone()
}

/// AC1: blocked inside the venv step, a search carries `installing_venv`
/// and the note; after the swap to python, no note.
#[tokio::test]
async fn a_blocked_venv_step_reports_installing_venv_then_no_note_after_the_swap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, switchable) = state_on_ort(dir.path()).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let (release, rx) = mpsc::channel();
    let ops: Arc<dyn PythonBootstrap> = Arc::new(FakeBootstrap(Venv::Block {
        entered: Arc::clone(&entered),
        release: Mutex::new(rx),
    }));
    let drive = tokio::spawn(drive_bootstrap(
        Arc::clone(&switchable),
        state.clone(),
        ops,
        1,
    ));
    tokio::time::timeout(Duration::from_secs(30), entered.notified())
        .await
        .expect("the fake venv step was entered");

    let during = search_embedder(&state).await;
    assert_eq!(during["active"], "ort", "{during}");
    assert_eq!(during["intended"], "python", "{during}");
    assert_eq!(during["bootstrap"], "installing_venv", "{during}");
    let note = during["note"].as_str().unwrap_or_default();
    assert!(
        note.contains("installs"),
        "the note names the install: {during}"
    );

    release.send(()).expect("release the venv step");
    let swapped = tokio::time::timeout(Duration::from_secs(30), drive)
        .await
        .expect("drive_bootstrap returns")
        .expect("drive task");
    assert!(swapped.is_some(), "the fake bootstrap swaps to python");
    let after = search_embedder(&state).await;
    assert_eq!(after["active"], "python", "{after}");
    assert_eq!(after["bootstrap"], "ready", "{after}");
    assert!(
        after["note"].is_null(),
        "no note once python serves: {after}"
    );
}

/// AC2 / Q5: an install that fails for good (here `TRUSTY_UV_FETCH=0` with
/// no uv) leaves ort serving, and the search note says degraded and why.
#[tokio::test]
async fn a_failed_bootstrap_carries_a_degraded_note_naming_the_cause() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, switchable) = state_on_ort(dir.path()).await;
    let ops: Arc<dyn PythonBootstrap> = Arc::new(FakeBootstrap(Venv::Fail));
    let swapped = drive_bootstrap(Arc::clone(&switchable), state.clone(), ops, 2).await;
    assert!(swapped.is_none(), "a venv failure is permanent");

    let embedder = search_embedder(&state).await;
    assert_eq!(embedder["active"], "ort", "{embedder}");
    assert_eq!(embedder["bootstrap"], "failed", "{embedder}");
    let note = embedder["note"].as_str().unwrap_or_default();
    assert!(
        note.starts_with("Degraded:") && note.contains("TRUSTY_UV_FETCH=0"),
        "the note says degraded and why: {embedder}"
    );
}
