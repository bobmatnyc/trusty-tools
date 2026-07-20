//! End-to-end: drive the Python/MPS sidecar through the REAL trusty-search
//! supervisor path, proving ZERO wire-code changes are required (epic #3524).
//!
//! Gated `#[ignore]` — it bootstraps a real venv (torch + sentence-transformers,
//! ~2-3 GB, network on a cold cache) and runs it on MPS/CPU. Run explicitly:
//!
//!   TRUSTY_RUN_PY_E2E=1 cargo test -p trusty-embedderd-py --test e2e -- --ignored
//!
//! It spawns the `trusty-embedderd-py` launcher exactly as the daemon does —
//! via `EmbedderSupervisor::spawn_stdio(launcher_path, SupervisorConfig)` — and
//! embeds a batch, asserting 384-dim unit-norm vectors come back.

use trusty_common::embedder_client::{EmbedderSupervisor, SupervisorConfig};

#[tokio::test]
#[ignore = "requires a built Python venv (torch); run with TRUSTY_RUN_PY_E2E=1 --ignored"]
async fn supervisor_spawns_launcher_and_embeds_batch() {
    if std::env::var("TRUSTY_RUN_PY_E2E").as_deref() != Ok("1") {
        eprintln!("skipping: set TRUSTY_RUN_PY_E2E=1 to run the real-venv e2e");
        return;
    }

    // 1. Ensure the venv (eager bootstrap, same call trusty-search makes).
    let layout = trusty_embedderd_py::ensure_venv().expect("venv bootstrap");
    let launcher = trusty_embedderd_py::locate_launcher_binary().expect("locate launcher");
    eprintln!(
        "e2e: launcher={} venv_python={}",
        launcher.display(),
        layout.venv_python.display()
    );

    // 2. Spawn through the REAL supervisor with a generous startup timeout
    //    (cold model load + MPS warmup). No changes to supervisor/stdio.
    let config = SupervisorConfig {
        startup_timeout_secs: 180,
        ..SupervisorConfig::default()
    };
    let (supervisor, client_slot, _pid) = EmbedderSupervisor::spawn_stdio(launcher, config)
        .await
        .expect("supervisor spawn_stdio");
    let _handle = supervisor.start_supervisor_task();

    // 3. Embed a batch through the live client slot.
    let client = client_slot.read().await.clone();
    let texts = vec![
        "fn authenticate(user: &str) -> bool".to_string(),
        "the quick brown fox jumps over the lazy dog".to_string(),
    ];
    let vecs = client.embed_batch(texts).await.expect("embed_batch");

    assert_eq!(vecs.len(), 2, "one vector per text");
    for v in &vecs {
        assert_eq!(v.len(), 384, "384-dim");
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-2, "unit-norm, got {norm}");
        assert!(v.iter().any(|x| *x != 0.0), "never all-zero");
    }
    eprintln!(
        "e2e: embedded {} vectors, all 384-dim unit-norm",
        vecs.len()
    );
}
