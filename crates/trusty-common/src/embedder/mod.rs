//! Shared text-embedding abstraction for trusty-* projects.
//!
//! Why: trusty-memory and trusty-search both shipped near-identical
//! `Embedder` traits and `FastEmbedder` implementations, with subtle
//! drift (cache vs no-cache, sync vs async warmup, `dim()` vs `dimension()`).
//! Centralising fixes one bug in one place and lets future consumers pick up
//! the embedder for free.
//!
//! What: an async `Embedder` trait with `embed_batch` as the single primitive
//! (single-text embed is a free helper), plus a production `FastEmbedder`
//! (fastembed-rs, all-MiniLM-L6-v2, 384-d) with LRU caching and ORT warmup,
//! and a `MockEmbedder` test double behind the `embedder-test-support`
//! feature.
//!
//! Test: `cargo test -p trusty-common --features embedder,embedder-test-support`
//! covers shape, cache hits, and the mock embedder. ONNX-backed tests are
//! `#[ignore]` to keep CI under one cargo-feature umbrella.

// Issue #54: opt-in Candle BERT backend. Lives in its own submodule behind
// the `embedder-candle` feature so the default fastembed/ORT build never
// pays the candle compile cost.
#[cfg(feature = "embedder-candle")]
pub mod candle_embedder;
#[cfg(feature = "embedder-candle")]
pub use candle_embedder::{CandleEmbedder, CandleEmbedderError};

/// Portable resident-set-size (RSS) measurement helper.
///
/// Why: The candle Metal validation harness and any future memory-budget
/// tooling need a portable way to observe in-process RSS without per-OS
/// glue. Absorbed from the former `trusty-embedder` shim crate.
/// What: Exposes `rss::current_rss_bytes()` — a thin `sysinfo` wrapper.
/// Test: `rss::tests::rss_is_nonzero` and `rss_is_under_64gb` cover the
/// basic sanity invariants.
pub mod rss;

mod fast_embedder;
mod types;

#[cfg(any(test, feature = "embedder-test-support"))]
mod mock;

pub use fast_embedder::FastEmbedder;
pub use types::{
    CudaOptions, DEFAULT_CACHE_CAPACITY, DEFAULT_CUDA_GPU_MEM_LIMIT_BYTES,
    DEFAULT_ORT_INTER_THREADS, DEFAULT_ORT_INTRA_THREADS, EMBED_DIM, Embedder, ExecutionProvider,
    OrtThreadingOptions, embed_one, resolve_cuda_options, resolve_expected_provider,
    resolve_fastembed_cache_dir, resolve_ort_threading_options,
};

#[cfg(any(test, feature = "embedder-test-support"))]
pub use mock::MockEmbedder;

#[cfg(test)]
mod tests {
    use super::*;

    /// Process-global lock guarding every test in this module that mutates
    /// the process environment. Tests run in parallel by default, and
    /// concurrent calls into `setenv`/`getenv` (even on disjoint keys) are
    /// not safe across libc implementations — Rust 2024 reflects this by
    /// marking `std::env::set_var` `unsafe`. One shared lock across all
    /// env-touching tests in this binary is the simplest correct answer.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Why: GH #58 — launchd runs trusty-memory with a read-only `TMPDIR`,
    /// and fastembed's default `./.fastembed_cache` is also unwritable from
    /// the agent's working directory. Both env vars (`FASTEMBED_CACHE_DIR`,
    /// `FASTEMBED_CACHE_PATH`) and the `$HOME/.cache/fastembed` fallback must
    /// be honoured in that priority order. If the resolver regresses to
    /// `TMPDIR`, the HTTP daemon will silently fail to initialise embeddings
    /// for every install.
    /// What: serialises env mutation, exercises all three layers of the
    /// preference order, and asserts the resolver returns the expected path
    /// for each.
    /// Test: this test.
    #[test]
    fn resolve_fastembed_cache_dir_prefers_env_vars() {
        let _g = ENV_LOCK.lock().unwrap();

        let prev_dir = std::env::var("FASTEMBED_CACHE_DIR").ok();
        let prev_path = std::env::var("FASTEMBED_CACHE_PATH").ok();

        // SAFETY: serialised under ENV_LOCK; no other thread observes the
        // mutation. Required because Rust 2024 marks env mutation `unsafe`.
        unsafe {
            std::env::set_var("FASTEMBED_CACHE_DIR", "/tmp/fast-dir-test");
            std::env::remove_var("FASTEMBED_CACHE_PATH");
        }
        assert_eq!(
            resolve_fastembed_cache_dir(),
            std::path::PathBuf::from("/tmp/fast-dir-test"),
            "FASTEMBED_CACHE_DIR must win when set"
        );

        unsafe {
            std::env::remove_var("FASTEMBED_CACHE_DIR");
            std::env::set_var("FASTEMBED_CACHE_PATH", "/tmp/fast-path-test");
        }
        assert_eq!(
            resolve_fastembed_cache_dir(),
            std::path::PathBuf::from("/tmp/fast-path-test"),
            "FASTEMBED_CACHE_PATH must be honoured when FASTEMBED_CACHE_DIR is unset"
        );

        unsafe {
            std::env::remove_var("FASTEMBED_CACHE_DIR");
            std::env::remove_var("FASTEMBED_CACHE_PATH");
        }
        if let Some(home) = dirs::home_dir() {
            assert_eq!(
                resolve_fastembed_cache_dir(),
                home.join(".cache").join("fastembed"),
                "must fall back to $HOME/.cache/fastembed when no env vars set"
            );
        }

        // Restore for sibling tests.
        unsafe {
            match prev_dir {
                Some(v) => std::env::set_var("FASTEMBED_CACHE_DIR", v),
                None => std::env::remove_var("FASTEMBED_CACHE_DIR"),
            }
            match prev_path {
                Some(v) => std::env::set_var("FASTEMBED_CACHE_PATH", v),
                None => std::env::remove_var("FASTEMBED_CACHE_PATH"),
            }
        }
    }

    /// RAII helper: set or clear a `TRUSTY_GPU_MEM_LIMIT_*` env var for the
    /// duration of a test and restore it on drop. Keeps the CUDA-option tests
    /// from leaking state into sibling tests that share the process env.
    ///
    /// Why: env vars are process-global; without restore, a stray
    /// `TRUSTY_GPU_MEM_LIMIT_*` would skew every later `resolve_cuda_options`
    /// call in the same binary.
    /// What: captures the prior value, applies the new one (or removes it for
    /// `None`), and reinstates the prior value on `Drop`.
    /// Test: used by the `cuda_options_*` tests below.
    struct EnvVarGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvVarGuard {
        fn apply(key: &'static str, value: Option<&str>) -> Self {
            let prev = std::env::var(key).ok();
            // SAFETY: every caller holds `ENV_LOCK`, so no other thread reads
            // or writes the environment concurrently.
            unsafe {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
            Self { key, prev }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: same single-threaded-under-ENV_LOCK invariant as `apply`.
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    /// Why: issue #600 — with no operator override the CUDA arena cap must
    /// default to the 16 GB-T4-safe 12 GiB ceiling so the daemon never OOMs a
    /// T4 out of the box and the `TRUSTY_MAX_BATCH_SIZE=32` workaround is no
    /// longer required.
    /// What: clears both env knobs and asserts the default byte value.
    /// Test: this test.
    #[test]
    fn cuda_options_default_limit() {
        let _g = ENV_LOCK.lock().unwrap();
        let _b = EnvVarGuard::apply("TRUSTY_GPU_MEM_LIMIT_BYTES", None);
        let _m = EnvVarGuard::apply("TRUSTY_GPU_MEM_LIMIT_MB", None);
        assert_eq!(
            resolve_cuda_options().gpu_mem_limit_bytes,
            DEFAULT_CUDA_GPU_MEM_LIMIT_BYTES,
            "default CUDA gpu_mem_limit must be 12 GiB when no env knob is set"
        );
        assert_eq!(DEFAULT_CUDA_GPU_MEM_LIMIT_BYTES, 12 * 1024 * 1024 * 1024);
    }

    #[test]
    fn cuda_options_bytes_env() {
        let _g = ENV_LOCK.lock().unwrap();
        let _b = EnvVarGuard::apply("TRUSTY_GPU_MEM_LIMIT_BYTES", Some("8589934592"));
        let _m = EnvVarGuard::apply("TRUSTY_GPU_MEM_LIMIT_MB", None);
        assert_eq!(
            resolve_cuda_options().gpu_mem_limit_bytes,
            8_589_934_592,
            "explicit byte limit must be used verbatim"
        );
    }

    #[test]
    fn cuda_options_mb_env() {
        let _g = ENV_LOCK.lock().unwrap();
        let _b = EnvVarGuard::apply("TRUSTY_GPU_MEM_LIMIT_BYTES", None);
        let _m = EnvVarGuard::apply("TRUSTY_GPU_MEM_LIMIT_MB", Some("4096"));
        assert_eq!(
            resolve_cuda_options().gpu_mem_limit_bytes,
            4096usize * 1024 * 1024,
            "MB limit must be scaled to bytes"
        );
    }

    #[test]
    fn cuda_options_bytes_takes_precedence() {
        let _g = ENV_LOCK.lock().unwrap();
        let _b = EnvVarGuard::apply("TRUSTY_GPU_MEM_LIMIT_BYTES", Some("1073741824"));
        let _m = EnvVarGuard::apply("TRUSTY_GPU_MEM_LIMIT_MB", Some("4096"));
        assert_eq!(
            resolve_cuda_options().gpu_mem_limit_bytes,
            1_073_741_824,
            "BYTES knob must take precedence over MB"
        );
    }

    #[test]
    fn cuda_options_ignores_malformed() {
        let _g = ENV_LOCK.lock().unwrap();
        {
            let _b = EnvVarGuard::apply("TRUSTY_GPU_MEM_LIMIT_BYTES", Some("not-a-number"));
            let _m = EnvVarGuard::apply("TRUSTY_GPU_MEM_LIMIT_MB", Some("2048"));
            assert_eq!(
                resolve_cuda_options().gpu_mem_limit_bytes,
                2048usize * 1024 * 1024,
                "malformed BYTES must fall through to a valid MB knob"
            );
        }
        {
            let _b = EnvVarGuard::apply("TRUSTY_GPU_MEM_LIMIT_BYTES", Some("0"));
            let _m = EnvVarGuard::apply("TRUSTY_GPU_MEM_LIMIT_MB", Some("nope"));
            assert_eq!(
                resolve_cuda_options().gpu_mem_limit_bytes,
                DEFAULT_CUDA_GPU_MEM_LIMIT_BYTES,
                "zero BYTES + malformed MB must fall back to the safe default"
            );
        }
    }

    /// Why: #1542 — the CUDA deferred-embed deadlock is fixed by pinning ORT's
    /// intra-/inter-op threads to 1 and disabling intra-op spinning. With no
    /// operator override the resolver must default to that safe single-threaded,
    /// no-spin profile so the daemon never reproduces the 8-ORT-thread barrier
    /// deadlock out of the box.
    /// What: clears all three knobs and asserts the defaults.
    /// Test: this test.
    #[test]
    fn ort_threading_defaults() {
        let _g = ENV_LOCK.lock().unwrap();
        let _i = EnvVarGuard::apply("TRUSTY_ORT_INTRA_THREADS", None);
        let _e = EnvVarGuard::apply("TRUSTY_ORT_INTER_THREADS", None);
        let _s = EnvVarGuard::apply("TRUSTY_ORT_ALLOW_SPINNING", None);

        let opts = resolve_ort_threading_options();
        assert_eq!(
            opts.intra_threads, DEFAULT_ORT_INTRA_THREADS,
            "default intra-op threads must be 1 to avoid the #1542 barrier deadlock"
        );
        assert_eq!(opts.inter_threads, DEFAULT_ORT_INTER_THREADS);
        assert!(
            !opts.allow_spinning,
            "spinning must default to off — it is the busy-wait half of the deadlock"
        );
        assert_eq!(DEFAULT_ORT_INTRA_THREADS, 1);
        assert_eq!(DEFAULT_ORT_INTER_THREADS, 1);
    }

    /// Why: operators on hosts proven free of the ORT barrier bug may want to
    /// raise the thread counts; the knobs must be honoured so the safe default
    /// is overridable without a code change.
    /// What: sets explicit positive thread counts and asserts they pass through.
    /// Test: this test.
    #[test]
    fn ort_threading_reads_env() {
        let _g = ENV_LOCK.lock().unwrap();
        let _i = EnvVarGuard::apply("TRUSTY_ORT_INTRA_THREADS", Some("4"));
        let _e = EnvVarGuard::apply("TRUSTY_ORT_INTER_THREADS", Some("2"));
        let _s = EnvVarGuard::apply("TRUSTY_ORT_ALLOW_SPINNING", None);

        let opts = resolve_ort_threading_options();
        assert_eq!(
            opts.intra_threads, 4,
            "explicit intra-op count must be used"
        );
        assert_eq!(
            opts.inter_threads, 2,
            "explicit inter-op count must be used"
        );
        assert!(!opts.allow_spinning);
    }

    /// Why: a malformed or zero thread count must never silently disable
    /// parallelism control or panic — it must fall back to the safe default so
    /// the deadlock fix still holds.
    /// What: feeds non-numeric and zero values and asserts the defaults win.
    /// Test: this test.
    #[test]
    fn ort_threading_ignores_malformed() {
        let _g = ENV_LOCK.lock().unwrap();
        let _i = EnvVarGuard::apply("TRUSTY_ORT_INTRA_THREADS", Some("not-a-number"));
        let _e = EnvVarGuard::apply("TRUSTY_ORT_INTER_THREADS", Some("0"));
        let _s = EnvVarGuard::apply("TRUSTY_ORT_ALLOW_SPINNING", None);

        let opts = resolve_ort_threading_options();
        assert_eq!(
            opts.intra_threads, DEFAULT_ORT_INTRA_THREADS,
            "malformed intra-op count must fall back to the safe default"
        );
        assert_eq!(
            opts.inter_threads, DEFAULT_ORT_INTER_THREADS,
            "zero inter-op count must fall back to the safe default"
        );
    }

    /// Why: spinning is opt-in and only safe under constant inference; the knob
    /// must accept the common truthy spellings and treat everything else
    /// (including unset) as disabled.
    /// What: asserts a truthy value enables spinning while a non-truthy value
    /// leaves it off.
    /// Test: this test.
    #[test]
    fn ort_threading_spinning_truthy() {
        let _g = ENV_LOCK.lock().unwrap();
        let _i = EnvVarGuard::apply("TRUSTY_ORT_INTRA_THREADS", None);
        let _e = EnvVarGuard::apply("TRUSTY_ORT_INTER_THREADS", None);

        {
            let _s = EnvVarGuard::apply("TRUSTY_ORT_ALLOW_SPINNING", Some("TRUE"));
            assert!(
                resolve_ort_threading_options().allow_spinning,
                "a truthy value (case-insensitive) must enable spinning"
            );
        }
        {
            let _s = EnvVarGuard::apply("TRUSTY_ORT_ALLOW_SPINNING", Some("maybe"));
            assert!(
                !resolve_ort_threading_options().allow_spinning,
                "a non-truthy value must leave spinning disabled"
            );
        }
    }

    #[test]
    fn resolve_expected_provider_forces_cpu() {
        let _g = ENV_LOCK.lock().unwrap();
        let _d = EnvVarGuard::apply("TRUSTY_DEVICE", Some("CPU"));
        assert_eq!(resolve_expected_provider(), ExecutionProvider::Cpu);
    }

    #[test]
    fn resolve_expected_provider_default_matches_platform() {
        let _g = ENV_LOCK.lock().unwrap();
        let _d = EnvVarGuard::apply("TRUSTY_DEVICE", None);
        let _u = EnvVarGuard::apply("TRUSTY_COREML_COMPUTE_UNITS", None);

        let got = resolve_expected_provider();

        #[cfg(feature = "embedder-cuda")]
        let expected = ExecutionProvider::Cuda;
        #[cfg(all(
            not(feature = "embedder-cuda"),
            target_arch = "aarch64",
            target_os = "macos"
        ))]
        let expected = ExecutionProvider::CoreMLAne;
        #[cfg(all(
            not(feature = "embedder-cuda"),
            not(all(target_arch = "aarch64", target_os = "macos"))
        ))]
        let expected = ExecutionProvider::Cpu;

        assert_eq!(
            got, expected,
            "predicted provider must match init_options for this build/platform"
        );
    }

    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    #[cfg(not(feature = "embedder-cuda"))]
    #[test]
    fn resolve_expected_provider_coreml_units() {
        let _g = ENV_LOCK.lock().unwrap();
        let _d = EnvVarGuard::apply("TRUSTY_DEVICE", None);
        {
            let _u = EnvVarGuard::apply("TRUSTY_COREML_COMPUTE_UNITS", Some("all"));
            assert_eq!(resolve_expected_provider(), ExecutionProvider::CoreML);
        }
        {
            let _u = EnvVarGuard::apply("TRUSTY_COREML_COMPUTE_UNITS", None);
            assert_eq!(resolve_expected_provider(), ExecutionProvider::CoreMLAne);
        }
    }

    #[tokio::test]
    async fn mock_embedder_round_trip() {
        let e = MockEmbedder::new(EMBED_DIM);
        assert_eq!(e.dimension(), EMBED_DIM);
        let v = embed_one(&e, "hello").await.unwrap();
        assert_eq!(v.len(), EMBED_DIM);
        let batch = e
            .embed_batch(&["a".to_string(), "b".to_string()])
            .await
            .unwrap();
        assert_eq!(batch.len(), 2);
        assert_ne!(batch[0], batch[1]);
    }

    #[tokio::test]
    async fn mock_embedder_empty_input_returns_empty() {
        let e = MockEmbedder::new(EMBED_DIM);
        let v = e.embed_batch(&[]).await.unwrap();
        assert!(v.is_empty());
    }

    #[tokio::test]
    #[ignore]
    async fn fastembed_returns_correct_dim() {
        let e = FastEmbedder::new().await.unwrap();
        assert_eq!(e.dimension(), 384);
        let v = embed_one(&e, "fn authenticate(user: &str) -> bool")
            .await
            .unwrap();
        assert_eq!(v.len(), 384);
        assert!(v.iter().any(|x| *x != 0.0));
    }

    #[tokio::test]
    #[ignore]
    async fn fastembed_cache_hit_is_idempotent() {
        let e = FastEmbedder::new().await.unwrap();
        let v1 = embed_one(&e, "cached").await.unwrap();
        let v2 = embed_one(&e, "cached").await.unwrap();
        assert_eq!(v1, v2);
    }

    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    #[test]
    fn trusty_device_cpu_disables_coreml_on_apple_silicon() {
        use crate::embedder::fast_embedder::FastEmbedder as FE;
        let _guard = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("TRUSTY_DEVICE").ok();
        unsafe { std::env::set_var("TRUSTY_DEVICE", "cpu") };

        let (_opts, provider) = FE::init_options(fastembed::EmbeddingModel::AllMiniLML6V2Q);
        assert_eq!(
            provider,
            ExecutionProvider::Cpu,
            "TRUSTY_DEVICE=cpu must suppress CoreML EP on Apple Silicon"
        );

        unsafe {
            match prev {
                Some(v) => std::env::set_var("TRUSTY_DEVICE", v),
                None => std::env::remove_var("TRUSTY_DEVICE"),
            }
        }
    }

    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    #[test]
    fn default_apple_silicon_uses_coreml_ane() {
        use crate::embedder::fast_embedder::FastEmbedder as FE;
        let _guard = ENV_LOCK.lock().unwrap();

        let prev_device = std::env::var("TRUSTY_DEVICE").ok();
        let prev_units = std::env::var("TRUSTY_COREML_COMPUTE_UNITS").ok();
        unsafe {
            std::env::remove_var("TRUSTY_DEVICE");
            std::env::remove_var("TRUSTY_COREML_COMPUTE_UNITS");
        }

        let (_opts, provider) = FE::init_options(fastembed::EmbeddingModel::AllMiniLML6V2Q);
        assert_eq!(
            provider,
            ExecutionProvider::CoreMLAne,
            "default behaviour on Apple Silicon must register CoreML(ANE) — the OOM-safe replacement for CoreML(All)"
        );

        unsafe {
            match prev_device {
                Some(v) => std::env::set_var("TRUSTY_DEVICE", v),
                None => std::env::remove_var("TRUSTY_DEVICE"),
            }
            match prev_units {
                Some(v) => std::env::set_var("TRUSTY_COREML_COMPUTE_UNITS", v),
                None => std::env::remove_var("TRUSTY_COREML_COMPUTE_UNITS"),
            }
        }
    }

    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    #[test]
    fn coreml_compute_units_all_opt_in() {
        use crate::embedder::fast_embedder::FastEmbedder as FE;
        let _guard = ENV_LOCK.lock().unwrap();

        let prev_device = std::env::var("TRUSTY_DEVICE").ok();
        let prev_units = std::env::var("TRUSTY_COREML_COMPUTE_UNITS").ok();
        unsafe {
            std::env::remove_var("TRUSTY_DEVICE");
            std::env::set_var("TRUSTY_COREML_COMPUTE_UNITS", "all");
        }

        let (_opts, provider) = FE::init_options(fastembed::EmbeddingModel::AllMiniLML6V2Q);
        assert_eq!(
            provider,
            ExecutionProvider::CoreML,
            "TRUSTY_COREML_COMPUTE_UNITS=all must select the CoreML(All) tag"
        );

        unsafe {
            match prev_device {
                Some(v) => std::env::set_var("TRUSTY_DEVICE", v),
                None => std::env::remove_var("TRUSTY_DEVICE"),
            }
            match prev_units {
                Some(v) => std::env::set_var("TRUSTY_COREML_COMPUTE_UNITS", v),
                None => std::env::remove_var("TRUSTY_COREML_COMPUTE_UNITS"),
            }
        }
    }

    /// Why: issue #2111 — the fallback *decision* must be verifiable without a
    /// real ORT/CoreML runtime. A successful init must never fall back or
    /// poison the known-bad cache.
    /// What: asserts `decide_fallback(Success) == (false, false)`.
    /// Test: this test.
    #[test]
    fn fallback_decision_success_keeps_provider() {
        use crate::embedder::fast_embedder::{InitOutcome, decide_fallback};
        assert_eq!(decide_fallback(InitOutcome::Success), (false, false));
    }

    /// Why: a fast `Err` (pre-existing #763 behaviour) is not proof the
    /// provider will misbehave again, so it must fall back to CPU for this
    /// attempt WITHOUT poisoning the process-wide known-bad cache — the next
    /// full `FastEmbedder::new()` should still retry the accelerated
    /// provider.
    /// What: asserts `decide_fallback(FastError) == (true, false)`.
    /// Test: this test.
    #[test]
    fn fallback_decision_fast_error_falls_back_without_poisoning() {
        use crate::embedder::fast_embedder::{InitOutcome, decide_fallback};
        assert_eq!(decide_fallback(InitOutcome::FastError), (true, false));
    }

    /// Why: issue #2111 — a `Hung` outcome means an OS thread is permanently
    /// stuck inside ORT/CoreML with no cancellation point. Falling back
    /// without poisoning the cache would leak one more blocked thread on
    /// every subsequent retry (e.g. each ~5-minute dream cycle), so a hang
    /// MUST poison `COREML_KNOWN_BAD` so it is never attempted again this
    /// process.
    /// What: asserts `decide_fallback(Hung) == (true, true)`.
    /// Test: this test.
    #[test]
    fn fallback_decision_hang_falls_back_and_poisons() {
        use crate::embedder::fast_embedder::{InitOutcome, decide_fallback};
        assert_eq!(decide_fallback(InitOutcome::Hung), (true, true));
    }

    /// Why: issue #2111 — the CoreML hang-detection bound must default to a
    /// documented, sensible value (60 s) so a hang is caught well before the
    /// outer `TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS` (default 180 s) gives up.
    /// What: clears the env override and asserts the default duration.
    /// Test: this test.
    #[test]
    fn coreml_init_timeout_default() {
        use crate::embedder::fast_embedder::{
            DEFAULT_COREML_INIT_TIMEOUT_SECS, coreml_init_timeout,
        };
        let _g = ENV_LOCK.lock().unwrap();
        let _t = EnvVarGuard::apply("TRUSTY_COREML_INIT_TIMEOUT_SECS", None);
        assert_eq!(
            coreml_init_timeout(),
            std::time::Duration::from_secs(DEFAULT_COREML_INIT_TIMEOUT_SECS)
        );
        assert_eq!(DEFAULT_COREML_INIT_TIMEOUT_SECS, 60);
    }

    /// Why: operators on a host with a legitimately slow (but working) CoreML
    /// cold-compile must be able to raise the bound without a code change.
    /// What: sets an explicit override and asserts it is honoured; also
    /// checks a malformed value falls back to the default.
    /// Test: this test.
    #[test]
    fn coreml_init_timeout_reads_env() {
        use crate::embedder::fast_embedder::{
            DEFAULT_COREML_INIT_TIMEOUT_SECS, coreml_init_timeout,
        };
        let _g = ENV_LOCK.lock().unwrap();
        {
            let _t = EnvVarGuard::apply("TRUSTY_COREML_INIT_TIMEOUT_SECS", Some("120"));
            assert_eq!(coreml_init_timeout(), std::time::Duration::from_secs(120));
        }
        {
            let _t = EnvVarGuard::apply("TRUSTY_COREML_INIT_TIMEOUT_SECS", Some("not-a-number"));
            assert_eq!(
                coreml_init_timeout(),
                std::time::Duration::from_secs(DEFAULT_COREML_INIT_TIMEOUT_SECS),
                "malformed override must fall back to the default"
            );
        }
        {
            let _t = EnvVarGuard::apply("TRUSTY_COREML_INIT_TIMEOUT_SECS", Some("0"));
            assert_eq!(
                coreml_init_timeout(),
                std::time::Duration::from_secs(DEFAULT_COREML_INIT_TIMEOUT_SECS),
                "zero override must fall back to the default"
            );
        }
    }

    /// Why: zero vectors from a CUDA EP silent fallback would be stored in the
    /// HNSW index and corrupt all similarity searches.
    /// What: asserts `is_zero_vector` fires on an all-zero vector and not on
    /// a non-zero vector.
    /// Test: `zero_vector_guard_rejects_all_zero_batch`.
    #[test]
    fn zero_vector_guard_rejects_all_zero_batch() {
        let zero_vec: Vec<f32> = vec![0.0; EMBED_DIM];
        let non_zero_vec: Vec<f32> = {
            let mut v = vec![0.0_f32; EMBED_DIM];
            v[0] = 1.0;
            v
        };
        assert!(
            types::is_zero_vector(&zero_vec),
            "synthetic zero vector must be detected by is_zero_vector"
        );
        assert!(
            !types::is_zero_vector(&non_zero_vec),
            "non-zero vector must NOT be detected by is_zero_vector"
        );
        let mock = MockEmbedder::new(EMBED_DIM);
        let hash_result = mock.hash_to_vec("some text");
        assert!(
            !types::is_zero_vector(&hash_result),
            "MockEmbedder must produce non-zero vectors for non-empty input"
        );
    }
}
