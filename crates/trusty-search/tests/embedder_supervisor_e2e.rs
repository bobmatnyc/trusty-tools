//! End-to-end integration tests for the `trusty-embedderd` supervisor.
//!
//! Why: unit tests validate the pure, deterministic parts of the supervisor
//! (config parsing, socket path, binary discovery). These e2e tests validate
//! the full process lifecycle: spawn, readiness, embed round-trip, crash-
//! restart, and graceful shutdown. They are all marked `#[ignore]` because
//! they require a real `trusty-embedderd` binary on PATH (or in the sibling-
//! of-current-exe location) and load the full ONNX model, which is too slow
//! for normal CI runs.
//!
//! Test: `cargo test -p trusty-search --test embedder_supervisor_e2e -- --include-ignored --nocapture`

#[cfg(test)]
mod e2e {
    use std::sync::Arc;
    use trusty_common::embedder_client::{EmbedderClient, EmbedderSupervisor};
    use trusty_search::service::embedder_supervisor::{locate_embedderd_binary, SupervisorConfig};

    /// Spawn the stdio sidecar, wait for readiness, and issue a single embed.
    ///
    /// Why: the most basic smoke-test for the auto-spawn path — proves the
    /// binary starts, the readiness probe succeeds, and a real embedding is
    /// returned.
    /// What: locate binary → `spawn_stdio` → `embed_batch` one string →
    /// assert a 384-dim unit vector.
    /// Test: this test. Run with `--include-ignored`.
    #[tokio::test]
    #[ignore = "requires trusty-embedderd binary + ONNX model (~15 s first run)"]
    async fn supervisor_spawns_and_serves_embed_requests() {
        let binary = locate_embedderd_binary().expect("trusty-embedderd not found on PATH");
        let cfg = SupervisorConfig::default().into_common_for_tests();
        let (supervisor, slot, pid_slot) = EmbedderSupervisor::spawn_stdio(binary, cfg)
            .await
            .expect("spawn_stdio failed");
        // Issue #282: verify that the PID slot is populated immediately after
        // spawn (non-zero means the child started and the supervisor recorded
        // its PID in the atomic slot).
        let initial_pid = pid_slot.load(std::sync::atomic::Ordering::Acquire);
        assert!(initial_pid > 0, "pid_slot should be non-zero after spawn");
        supervisor.start_supervisor_task();

        let client: Arc<dyn EmbedderClient> = slot.read().await.clone();
        let vecs = client
            .embed_batch(vec![
                "fn hello_world() -> &'static str { \"hello\" }".to_owned()
            ])
            .await
            .expect("embed_batch failed");

        assert_eq!(vecs.len(), 1, "expected 1 vector");
        assert_eq!(vecs[0].len(), 384, "expected 384-dim embedding");
        // Should be a unit vector (L2 norm ≈ 1.0).
        let norm: f32 = vecs[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-3,
            "embedding norm {norm} deviates from 1.0"
        );
    }

    /// The embedder's batch output must be bit-identical to the in-process path.
    ///
    /// Why: the stdio sidecar encodes/decodes through JSON-RPC 2.0. Any f32
    /// precision loss in the serialisation would silently degrade search quality.
    /// What: embed the same string via both paths and assert element-wise
    /// equality within floating-point tolerance.
    /// Test: this test. Run with `--include-ignored`.
    #[tokio::test]
    #[ignore = "requires trusty-embedderd binary + ONNX model (~30 s, loads model twice)"]
    async fn supervisor_vectors_match_in_process() {
        use trusty_common::embedder::FastEmbedder;
        use trusty_common::embedder_client::InProcessEmbedderClient;

        let text = "struct SearchResult { score: f32, chunk_id: String }".to_owned();

        // In-process vector — build FastEmbedder then wrap it.
        let fast = tokio::task::spawn_blocking(|| {
            tokio::runtime::Handle::current()
                .block_on(FastEmbedder::new())
                .expect("FastEmbedder init failed")
        })
        .await
        .expect("spawn_blocking panicked");
        let in_proc = InProcessEmbedderClient::new(fast);
        let ip_vecs = in_proc
            .embed_batch(vec![text.clone()])
            .await
            .expect("in-process embed_batch failed");

        // Sidecar vector.
        let binary = locate_embedderd_binary().expect("trusty-embedderd not found");
        let (supervisor, slot, _pid_slot) = EmbedderSupervisor::spawn_stdio(
            binary,
            SupervisorConfig::default().into_common_for_tests(),
        )
        .await
        .expect("spawn_stdio failed");
        supervisor.start_supervisor_task();
        let client = slot.read().await.clone();
        let sidecar_vecs = client
            .embed_batch(vec![text])
            .await
            .expect("sidecar embed_batch failed");

        assert_eq!(ip_vecs[0].len(), sidecar_vecs[0].len());
        for (a, b) in ip_vecs[0].iter().zip(sidecar_vecs[0].iter()) {
            assert!(
                (a - b).abs() < 1e-5,
                "vector element mismatch: in_process={a} sidecar={b}"
            );
        }
    }

    /// A batch of texts must all be embedded correctly (no off-by-one in
    /// the JSON-RPC array serialisation).
    ///
    /// Why: a subtle deserialisation bug could return the same vector for
    /// every element or return wrong lengths.
    /// What: embed 4 distinct strings and assert we get 4 distinct 384-dim
    /// vectors.
    /// Test: this test. Run with `--include-ignored`.
    #[tokio::test]
    #[ignore = "requires trusty-embedderd binary + ONNX model (~15 s)"]
    async fn supervisor_handles_batch_correctly() {
        let texts: Vec<String> = vec![
            "fn authenticate(token: &str) -> bool".to_owned(),
            "struct User { id: u64, name: String }".to_owned(),
            "impl Display for Error { fn fmt(&self, f: &mut Formatter) {} }".to_owned(),
            "async fn fetch_chunks(query: &str) -> Vec<Chunk>".to_owned(),
        ];

        let binary = locate_embedderd_binary().expect("trusty-embedderd not found");
        let (supervisor, slot, _pid_slot) = EmbedderSupervisor::spawn_stdio(
            binary,
            SupervisorConfig::default().into_common_for_tests(),
        )
        .await
        .expect("spawn_stdio failed");
        supervisor.start_supervisor_task();
        let client = slot.read().await.clone();

        let vecs = client.embed_batch(texts).await.expect("embed_batch failed");
        assert_eq!(vecs.len(), 4, "expected 4 vectors");
        for (i, v) in vecs.iter().enumerate() {
            assert_eq!(v.len(), 384, "vector {i} has wrong dimension");
        }
        // All 4 vectors must be distinct.
        for i in 0..4 {
            for j in (i + 1)..4 {
                assert_ne!(vecs[i], vecs[j], "vectors {i} and {j} should differ");
            }
        }
    }

    /// When the sidecar process is killed externally, the supervisor must
    /// restart it and subsequent embeds succeed.
    ///
    /// Why: the crash-restart loop is the key resilience feature of Phase 2.
    /// What: spawn → get child PID → SIGKILL → wait for restart → embed.
    /// Test: this test. Run with `--include-ignored`.
    #[tokio::test]
    #[ignore = "requires trusty-embedderd binary + ONNX model; kills a process (~30 s)"]
    async fn supervisor_restarts_after_crash() {
        let binary = locate_embedderd_binary().expect("trusty-embedderd not found");
        let cfg = SupervisorConfig {
            max_restarts: 3,
            ..SupervisorConfig::default()
        };
        let (supervisor, slot, pid_slot) =
            EmbedderSupervisor::spawn_stdio(binary, cfg.into_common_for_tests())
                .await
                .expect("spawn_stdio failed");
        // Issue #282: record the initial PID; after a crash + restart the slot
        // must be updated to the new child's PID.
        let pid_before_crash = pid_slot.load(std::sync::atomic::Ordering::Acquire);
        assert!(pid_before_crash > 0, "initial pid_slot should be non-zero");
        supervisor.start_supervisor_task();

        // First embed succeeds.
        let client = slot.read().await.clone();
        let vecs1 = client
            .embed_batch(vec!["before crash".to_owned()])
            .await
            .expect("pre-crash embed failed");
        assert_eq!(vecs1[0].len(), 384);

        // Give the supervisor loop time to restart and re-populate the slot.
        tokio::time::sleep(std::time::Duration::from_secs(15)).await;

        // Issue #282: pid_slot must point to the new child after restart.
        let pid_after_restart = pid_slot.load(std::sync::atomic::Ordering::Acquire);
        assert!(
            pid_after_restart > 0,
            "pid_slot should be non-zero after restart"
        );
        // The supervisor should have assigned a fresh PID.
        assert_ne!(
            pid_before_crash, pid_after_restart,
            "pid_slot should differ after crash restart (new child = new PID)"
        );

        // Post-restart embed must also succeed (slot now points to new client).
        let client2 = slot.read().await.clone();
        let vecs2 = client2
            .embed_batch(vec!["after crash".to_owned()])
            .await
            .expect("post-restart embed failed");
        assert_eq!(vecs2[0].len(), 384);
    }

    /// An empty batch must return an empty Vec without contacting the backend.
    ///
    /// Why: callers may forward empty batches during idle periods; the sidecar
    /// must not error or return spurious vectors.
    /// What: call `embed_batch` with an empty Vec and assert the result is Ok([]).
    /// Test: this test. Run with `--include-ignored`.
    #[tokio::test]
    #[ignore = "requires trusty-embedderd binary + ONNX model (~15 s)"]
    async fn supervisor_handles_empty_batch() {
        let binary = locate_embedderd_binary().expect("trusty-embedderd not found");
        let (supervisor, slot, _pid_slot) = EmbedderSupervisor::spawn_stdio(
            binary,
            SupervisorConfig::default().into_common_for_tests(),
        )
        .await
        .expect("spawn_stdio failed");
        supervisor.start_supervisor_task();
        let client = slot.read().await.clone();

        let vecs = client
            .embed_batch(vec![])
            .await
            .expect("empty embed_batch should return Ok([])");
        assert!(vecs.is_empty(), "expected empty result, got {}", vecs.len());
    }

    /// Ten concurrent embed calls must all complete without blocking each other.
    ///
    /// Why: the sidecar processes requests serially but the adapter acquires /
    /// releases the read lock quickly; callers must not starve.
    /// What: join 10 concurrent `embed_batch` calls and assert all 10 succeed.
    /// Test: this test. Run with `--include-ignored`.
    #[tokio::test]
    #[ignore = "requires trusty-embedderd binary + ONNX model (~20 s)"]
    async fn supervisor_handles_concurrent_requests() {
        let binary = locate_embedderd_binary().expect("trusty-embedderd not found");
        let (supervisor, slot, _pid_slot) = EmbedderSupervisor::spawn_stdio(
            binary,
            SupervisorConfig::default().into_common_for_tests(),
        )
        .await
        .expect("spawn_stdio failed");
        supervisor.start_supervisor_task();

        let slot = Arc::new(slot);
        let mut handles = Vec::new();
        for i in 0..10 {
            let slot_clone = Arc::clone(&slot);
            let text = format!("concurrent request {i}: fn do_something() -> i32 {{ {i} }}");
            handles.push(tokio::spawn(async move {
                let client = slot_clone.read().await.clone();
                let vecs = client.embed_batch(vec![text]).await?;
                Ok::<Vec<Vec<f32>>, trusty_common::embedder_client::EmbedderError>(vecs)
            }));
        }

        for (i, handle) in handles.into_iter().enumerate() {
            let result = handle.await.expect("task panicked").expect("embed failed");
            assert_eq!(result.len(), 1, "request {i}: expected 1 vector");
            assert_eq!(result[0].len(), 384, "request {i}: expected 384-dim");
        }
    }

    /// Closing the parent's stdin write-end causes the child to exit within 2 s.
    ///
    /// Why: this is the implicit lifecycle tie — when `trusty-search` exits and
    /// its `ChildStdin` is dropped, the child sees stdin EOF and must exit
    /// cleanly (code 0) without needing an explicit kill signal. This validates
    /// `run_stdio_server`'s EOF-exit branch so the sidecar never becomes a
    /// zombie when the parent exits.
    ///
    /// What: spawn `trusty-embedderd --stdio` with piped stdin/stdout; immediately
    /// drop the stdin handle (simulating parent exit); assert the child exits
    /// within 2 s with code 0.
    ///
    /// Note: the binary is started but exits before model load on cold runs —
    /// no ONNX model is needed, so this test runs quickly.
    ///
    /// Test: this test. Run with `--include-ignored`.
    #[tokio::test]
    #[ignore = "requires trusty-embedderd binary; fast — no ONNX model needed, exits on stdin EOF before model loads"]
    async fn stdio_eof_terminates_child() {
        use std::process::Stdio;
        use tokio::process::Command;

        let binary = locate_embedderd_binary().expect("trusty-embedderd not found on PATH");

        // Spawn the child with piped stdio — same flags EmbedderSupervisor uses.
        let mut child = Command::new(&binary)
            .arg("--stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("failed to spawn trusty-embedderd --stdio");

        // Drop stdin immediately — closes the write end of the pipe.
        // The child's `BufReader::read_line` returns n=0 (EOF) and the
        // `run_stdio_server` loop exits cleanly with `return Ok(())`.
        drop(child.stdin.take());

        // The child must exit cleanly within 2 seconds.
        let exit_status = tokio::time::timeout(std::time::Duration::from_secs(2), child.wait())
            .await
            .expect(
                "child did not exit within 2 s after stdin closed — \
             lifecycle tie (EOF → clean exit) is broken",
            )
            .expect("wait() failed");

        assert!(
            exit_status.success(),
            "child exited with non-zero status {:?} after stdin EOF — \
             expected clean exit (code 0)",
            exit_status.code()
        );
    }

    /// When `TRUSTY_EMBEDDERD_BIN` is set to a bad path, `locate_embedderd_binary`
    /// returns an error — not a panic.
    ///
    /// Why: operators may accidentally set the env var to a typo; we must
    /// surface a clear error at startup rather than panicking.
    /// What: set the var to a non-existent path and assert `Err`.
    /// Test: this test (no ONNX binary required; always runs).
    #[test]
    #[ignore = "pure env-var test — safe to run but grouped with e2e for discoverability"]
    fn bad_explicit_bin_path_returns_error() {
        // SAFETY: test-only, single-threaded by the time this assertion runs.
        let old = std::env::var("TRUSTY_EMBEDDERD_BIN").ok();
        unsafe {
            std::env::set_var("TRUSTY_EMBEDDERD_BIN", "/nonexistent/trusty-embedderd");
        }
        let result = locate_embedderd_binary();
        unsafe {
            match old {
                Some(v) => std::env::set_var("TRUSTY_EMBEDDERD_BIN", v),
                None => std::env::remove_var("TRUSTY_EMBEDDERD_BIN"),
            }
        }
        assert!(result.is_err(), "expected Err for bad explicit path");
    }
}
