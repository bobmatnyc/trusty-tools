//! Embedder construction and adapter types for `trusty-search start`.
//!
//! Why (issue #110 Phase 2 — stdio): `trusty-embedderd` is a required runtime
//! dependency. Running embedding in-process inside the search daemon couples
//! the ONNX model lifecycle to the daemon's memory budget and prevents
//! independent restart/upgrade of the embedding subsystem. The sidecar
//! architecture is a core design commitment, not an optional feature.
//!
//! What: `build_embedder()` reads `TRUSTY_EMBEDDER` and returns an
//! `Arc<dyn Embedder>` for the selected back-end. Adapter types bridge the
//! `trusty_common` `EmbedderClient` trait (Vec<String>) to the internal
//! `Embedder` trait (&[&str]). `tune_batch_size_for_provider` resets
//! `TRUSTY_MAX_BATCH_SIZE` when a GPU EP is detected.
//!
//! Test: exercised by `lazy_adapter_reports_resolved_provider` and
//! `uds_adapter_reports_resolved_provider` in `start/tests.rs`.

use std::path::PathBuf;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;

use anyhow::Result;

use crate::service::embedder_supervisor::{
    ActiveBackend, BackendKind, BootstrapState, SwitchableEmbedder,
};

/// Descriptive base model name surfaced on `ActiveBackend::model` (epic #3524
/// slice 6). Every backend arm below embeds into the same 384-dim MiniLM
/// family space — only quantization (see [`quantized_from_env`]) differs.
/// Purely informational: nothing in this PR reads it, PR-2's `/health` work
/// will.
pub(super) const EMBEDDER_MODEL_NAME: &str = "all-MiniLM-L6-v2";

/// Best-effort mirror of `trusty_common::embedder::fast_embedder`'s private
/// `resolve_default_embedding_model` env-var convention, for descriptive
/// metadata only (`ActiveBackend::quantized`).
///
/// Why: that resolver is `pub(super)` inside trusty-common and not reachable
/// from here; re-reading the same env var with the same convention gives an
/// accurate-enough answer for `/health` display without adding a new public
/// API surface for a single boolean. This function must never influence
/// which model is actually loaded — it only describes what the (unrelated)
/// real init already decided.
/// Test: `quantized_from_env_*` in `start/tests.rs`.
pub(super) fn quantized_from_env() -> bool {
    matches!(
        std::env::var("TRUSTY_EMBEDDER_MODEL")
            .ok()
            .as_deref()
            .map(|s| s.trim().to_ascii_lowercase())
            .as_deref(),
        Some("int8") | Some("quantized") | Some("q")
    )
}

/// Whether `ActiveBackend::quantized` should reflect [`quantized_from_env`]
/// for the given [`BackendKind`] (issue #3530 / #3493 P1).
///
/// Why: `TRUSTY_EMBEDDER_MODEL=int8` only ever selects between
/// `AllMiniLML6V2`/`AllMiniLML6V2Q` inside the ort/in-process `FastEmbedder`
/// path (`resolve_default_embedding_model` in trusty-common) — it has no
/// effect on the Python sidecar (which only ever loads its one pinned fp32
/// `sentence-transformers` model, see
/// `trusty_embed_sidecar.model.MODEL_NAME`) or on a manually managed remote
/// sidecar (whose model config the parent cannot see at all). Applying
/// `quantized_from_env()` unconditionally to every `BackendKind` would
/// report `quantized=true` for a python/remote backend just because the
/// operator happens to have `TRUSTY_EMBEDDER_MODEL=int8` set for an
/// unrelated ort run — never actually true.
/// What: `true` only for [`BackendKind::Ort`] and [`BackendKind::InProcess`]
/// — the two backends whose model selection actually reads that env var.
/// Test: `backend_respects_quantized_env_only_ort_and_in_process` in
/// `start/tests.rs`.
pub(super) fn backend_respects_quantized_env(kind: BackendKind) -> bool {
    matches!(kind, BackendKind::Ort | BackendKind::InProcess)
}

/// Which arm `build_embedder_raw` should take for `TRUSTY_EMBEDDER` unset /
/// `auto` (epic #3524 slice 6, PR 3/5).
///
/// Why: the unset/`auto` resolution needs to become platform-aware (Apple
/// Silicon defaults to the graceful hot-swap path once ship-gated) without
/// disturbing the explicit-value arms (`stdio`, `python`, `in-process`, …),
/// which are unaffected by this enum and keep matching directly on the raw
/// env string. Splitting resolution into its own pure function
/// ([`resolve_default_embedder_mode_for`]) makes the platform/env precedence
/// unit-testable without depending on the actual build target or process
/// environment.
/// What: one variant per arm `build_embedder_raw`'s `"" | "auto"` match takes.
/// Test: `resolve_default_embedder_mode_for_*` in `start/tests.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DefaultEmbedderMode {
    /// Build the ort stdio sidecar synchronously — unchanged pre-#3524
    /// behavior. Selected on every non-Apple-Silicon host, and on Apple
    /// Silicon whenever the graceful default is not ship-gated on
    /// (`TRUSTY_PY_DEFAULT` unset/falsy).
    Ort,
    /// Apple Silicon + `TRUSTY_PY_DEFAULT` enabled: serve on ort immediately,
    /// bootstrap the python/MPS sidecar in the background, then hot-swap
    /// (this PR's new arm). **Default OFF** — see `TRUSTY_PY_DEFAULT`'s doc.
    GracefulPython,
    /// `TRUSTY_EMBEDDER_PYTHON_EAGER` truthy: the existing eager, blocking
    /// `python` arm (identical to explicit `TRUSTY_EMBEDDER=python`) reached
    /// via unset/`auto` instead of an explicit value. Not gated to Apple
    /// Silicon — mirrors the explicit `python` arm, which has never been
    /// platform-gated (useful for testing the sidecar on any host).
    EagerPython,
}

/// Pure resolution logic for [`resolve_default_embedder_mode`] — unit
/// testable without depending on `cfg!(target_arch/target_os)` or real
/// process env vars.
///
/// Precedence: Apple-Silicon + the ship-gate flag enabled wins outright
/// (→ [`DefaultEmbedderMode::GracefulPython`]); otherwise an explicit
/// `TRUSTY_EMBEDDER_PYTHON_EAGER` opt-in wins (→
/// [`DefaultEmbedderMode::EagerPython`], any platform); otherwise the
/// unchanged ort default (→ [`DefaultEmbedderMode::Ort`]).
///
/// Note: `TRUSTY_EMBEDDER=stdio` (the permanent opt-out) never reaches this
/// function at all — `build_embedder_raw`'s match dispatches it straight to
/// `build_ort_stdio_sidecar()` before this resolution is consulted.
pub(super) fn resolve_default_embedder_mode_for(
    is_apple_silicon: bool,
    py_default_enabled: bool,
    eager_python: bool,
) -> DefaultEmbedderMode {
    if is_apple_silicon && py_default_enabled {
        return DefaultEmbedderMode::GracefulPython;
    }
    if eager_python {
        return DefaultEmbedderMode::EagerPython;
    }
    DefaultEmbedderMode::Ort
}

/// Resolve the `TRUSTY_EMBEDDER` unset/`auto` default, reading the real
/// build target and process environment (epic #3524 slice 6, PR 3/5).
///
/// Why: on Apple Silicon a torch/MPS sidecar embeds ~2.4x faster than ort
/// with numerically identical results (see the epic #3524 slice 2-4 spike);
/// once soaked, this should become the transparent default with zero
/// required user configuration. This PR wires the mechanism behind
/// `TRUSTY_PY_DEFAULT` (default OFF) so it can ship and be verified without
/// changing any real user's behavior — a later slice (PR-5) flips the
/// default on after the soak.
/// What: `cfg!(all(target_arch = "aarch64", target_os = "macos"))` for the
/// platform check, `TRUSTY_PY_DEFAULT` for the ship-gate, and
/// `TRUSTY_EMBEDDER_PYTHON_EAGER` for the eager escape hatch — delegates the
/// actual precedence to [`resolve_default_embedder_mode_for`].
/// Test: `resolve_default_embedder_mode_for_*` in `start/tests.rs` cover the
/// pure precedence; this wrapper has no independent branches to test.
pub(super) fn resolve_default_embedder_mode() -> DefaultEmbedderMode {
    resolve_default_embedder_mode_for(
        cfg!(all(target_arch = "aarch64", target_os = "macos")),
        truthy_env("TRUSTY_PY_DEFAULT"),
        truthy_env("TRUSTY_EMBEDDER_PYTHON_EAGER"),
    )
}

/// Shared truthy-env-var parser: `1`/`true`/`yes`/`on` (case-insensitive,
/// trimmed) → `true`; unset or anything else → `false`.
/// Test: `truthy_env_*` in `start/tests.rs`.
pub(super) fn truthy_env(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// Resolve the embedder back-end, wrap it in a [`SwitchableEmbedder`], and
/// return both the trait-object handle and the concrete switchable handle.
///
/// Why (epic #3524 slice 6 — PR 1/5): every existing owner of the embedder
/// `Arc` (embed-pool workers, `embedder_slot`, restore) captures its own
/// clone at construction time, so writing a fresh `Arc<dyn Embedder>`
/// somewhere else never reaches them. Wrapping the real backend in
/// `SwitchableEmbedder` here — once, at the single construction point — means
/// every one of those owners transparently observes a future hot-swap
/// (`SwitchableEmbedder::swap_to`, wired up by a later slice) with zero
/// call-site changes. This PR is a pure refactor: nothing calls `swap_to`
/// yet, so every code path behaves exactly as it did before.
///
/// What: delegates backend selection to [`build_embedder_raw`], reads the
/// resolved provider off the raw embedder, builds an [`ActiveBackend`]
/// snapshot (`bootstrap: NotApplicable` — no orchestrator exists yet), and
/// returns `(Arc<dyn Embedder>, Option<Arc<AtomicU32>>, Arc<SwitchableEmbedder>)`.
/// The first element is the `switchable` handle coerced to the trait object
/// (identical value, just a different static view) so existing call sites
/// keep working unchanged; the third element is the concrete handle PR-2
/// (`/health`) and PR-3 (the hot-swap orchestrator) need.
///
/// Test: `build_embedder_raw`'s existing coverage (`lazy_adapter_reports_resolved_provider`,
/// `uds_adapter_reports_resolved_provider`) is unaffected; `switchable.rs`
/// unit tests cover the wrapper itself.
///
/// The 4th return element (epic #3524 slice 6, PR 3/5) is `true` only when
/// `build_embedder_raw` took the new [`DefaultEmbedderMode::GracefulPython`]
/// arm — the signal `commands::start::daemon` uses to decide whether to
/// spawn the background bootstrap→hot-swap orchestrator right after
/// installing the returned `switchable` handle. Every pre-existing arm
/// returns `false`, so this PR ships as a no-op for every caller that
/// doesn't opt in via `TRUSTY_PY_DEFAULT`.
pub(super) async fn build_embedder() -> Result<(
    std::sync::Arc<dyn crate::core::Embedder>,
    Option<Arc<AtomicU32>>,
    Arc<SwitchableEmbedder>,
    bool,
)> {
    let (raw, pid_slot, kind, needs_graceful_bootstrap) = build_embedder_raw().await?;
    let provider = raw.provider();
    let active = ActiveBackend {
        kind,
        provider,
        model: EMBEDDER_MODEL_NAME.to_string(),
        // Issue #3530 / #3493 P1: only the ort/in-process `FastEmbedder`
        // path actually honours `TRUSTY_EMBEDDER_MODEL` — see
        // `backend_respects_quantized_env`'s doc for why.
        quantized: backend_respects_quantized_env(kind) && quantized_from_env(),
        // Epic #3524 slice 6 PR-3: the graceful-python arm starts the
        // background bootstrap immediately, so `/health` must report
        // `Bootstrapping` from the very first read rather than `NotApplicable`.
        bootstrap: if needs_graceful_bootstrap {
            BootstrapState::Bootstrapping
        } else {
            BootstrapState::NotApplicable
        },
    };
    let switchable = Arc::new(SwitchableEmbedder::new(raw, active));
    let embedder: Arc<dyn crate::core::Embedder> =
        Arc::clone(&switchable) as Arc<dyn crate::core::Embedder>;
    Ok((embedder, pid_slot, switchable, needs_graceful_bootstrap))
}

/// Resolve the embedder back-end and return the raw `Arc<dyn Embedder>` ready
/// for use, tagged with which [`BackendKind`] was constructed.
///
/// Why (issue #110 Phase 2 — stdio): `trusty-embedderd` is a required runtime
/// dependency. Running embedding in-process inside the search daemon couples
/// the ONNX model lifecycle to the daemon's memory budget and prevents
/// independent restart/upgrade of the embedding subsystem. The sidecar
/// architecture is a core design commitment, not an optional feature — silent
/// fallback to in-process would let users miss the new architecture entirely.
///
/// What: reads `TRUSTY_EMBEDDER` and dispatches:
///   - unset / `auto` / `stdio` → arm a `LazyEmbedderHandle` (issue #315,
///     deferred spawn — the child process starts on the first embed request,
///     not at daemon boot). Fails fast with an install hint if the binary is
///     not on PATH.
///   - `in-process`             → in-process FastEmbedder (explicit escape hatch
///     for tests / debugging — never silently activated)
///   - `http://…`               → HTTP remote (manually managed embedderd)
///   - `unix:/path`             → UDS remote (manually managed embedderd)
///   - `candle`                 → Candle Metal backend (feature-gated)
///
/// Test: run `trusty-search start` with `RUST_LOG=info` — the startup log must
/// contain `"embedderd supervisor armed, deferred spawn enabled"` before the
/// first request is served, and `"spawning trusty-embedderd"` only when the
/// first hybrid search or reindex arrives.
///
/// Returns `(embedder, embedderd_pid_slot, kind, needs_graceful_bootstrap)`.
/// `embedderd_pid_slot` is `Some` only for the stdio-sidecar path and holds
/// an `Arc<AtomicU32>` that the `LazyEmbedderHandle` keeps updated with the
/// current child OS PID (0 when no live process) so callers can sample the
/// sidecar's RSS without holding any mutex. Non-stdio paths return `None`.
/// `kind` (epic #3524 slice 6) tags which [`BackendKind`] was constructed so
/// [`build_embedder`] can build an accurate [`ActiveBackend`] snapshot around
/// it. `needs_graceful_bootstrap` (PR 3/5) is `true` only for the
/// `DefaultEmbedderMode::GracefulPython` arm — see [`build_embedder`]'s doc.
async fn build_embedder_raw() -> Result<(
    std::sync::Arc<dyn crate::core::Embedder>,
    Option<Arc<AtomicU32>>,
    BackendKind,
    bool,
)> {
    let trusty_embedder_env = std::env::var("TRUSTY_EMBEDDER").unwrap_or_default();

    // Issue #41 phase 4: candle Metal path (feature-gated, explicit opt-in).
    #[cfg(feature = "candle")]
    {
        if trusty_embedder_env == "candle" {
            let candle =
                tokio::task::spawn_blocking(crate::service::candle_embedder::CandleEmbedder::new)
                    .await
                    .map_err(|e| anyhow::anyhow!("candle embedder init task panicked: {e}"))??;
            let dim = candle.dimension();
            tracing::info!("embedder initialized: model=all-MiniLM-L6-v2 dim={dim} backend=candle");
            return Ok((
                std::sync::Arc::new(candle),
                None,
                BackendKind::Candle,
                false,
            ));
        }
    }

    match trusty_embedder_env.as_str() {
        // ── Explicit force-ort opt-out — the ONLY way to permanently pin
        // the ort sidecar on Apple Silicon once TRUSTY_PY_DEFAULT ships on
        // (epic #3524 slice 6, PR 3/5). Never routed through
        // `resolve_default_embedder_mode` below.
        "stdio" => build_ort_stdio_sidecar().map(|(e, p)| (e, p, BackendKind::Ort, false)),

        // ── unset / `auto` — platform-aware resolution (epic #3524 slice 6,
        // PR 3/5). `resolve_default_embedder_mode` decides between the
        // unchanged ort default, the new graceful-python background path, and
        // the existing eager-blocking python path (reachable here only via
        // `TRUSTY_EMBEDDER_PYTHON_EAGER`, mirroring explicit
        // `TRUSTY_EMBEDDER=python` below).
        "" | "auto" => match resolve_default_embedder_mode() {
            DefaultEmbedderMode::Ort => {
                build_ort_stdio_sidecar().map(|(e, p)| (e, p, BackendKind::Ort, false))
            }
            DefaultEmbedderMode::EagerPython => build_eager_python_embedder()
                .await
                .map(|(e, p, k)| (e, p, k, false)),
            DefaultEmbedderMode::GracefulPython => {
                // Epic #3524 slice 6 (PR 3/5): serve on ort IMMEDIATELY — the
                // exact same synchronous construction as the unchanged default
                // path — while the caller (`build_embedder`) marks the
                // resulting `ActiveBackend::bootstrap` `Bootstrapping` and
                // `commands::start::daemon` spawns the background
                // bootstrap→hot-swap orchestrator right after installing it.
                // Nothing here blocks the HTTP listener or startup.
                tracing::info!(
                    "embedder mode: graceful Apple-Silicon default (TRUSTY_PY_DEFAULT) — \
                     serving on the ort stdio sidecar immediately while the python/MPS \
                     sidecar bootstraps in the background; hot-swaps in when ready"
                );
                build_ort_stdio_sidecar().map(|(e, p)| (e, p, BackendKind::Ort, true))
            }
        },

        // ── Opt-in Python/MPS sidecar (epic #3524, slices 2-4) — DEFAULT-OFF ──
        //
        // Why: on Apple Silicon a torch/MPS sentence-transformers sidecar
        // embeds ~2.4x faster than the Rust ort path with numerically
        // identical results (the spike measured 561 emb/s end-to-end through
        // the real supervisor). Selection is strictly opt-in; default-on is a
        // LATER slice. Reuses `LazyEmbedderHandle` / `EmbedderSupervisor` with
        // ZERO changes to the supervisor/stdio/protocol wire code — the
        // launcher speaks the exact same JSON-RPC 2.0 stdio protocol.
        //
        // Robustness: eager-bootstrap the venv here (at `start`). On ANY
        // bootstrap or launcher-discovery failure, log a loud actionable
        // warning and FALL BACK to the Rust ort path so search never
        // hard-fails.
        "python" => build_eager_python_embedder()
            .await
            .map(|(e, p, k)| (e, p, k, false)),

        // ── In-process safety-valve ────────────────────────────────────────
        "in-process" | "local" => {
            tracing::info!("embedder mode: in-process (override via TRUSTY_EMBEDDER=in-process)");
            let embedder = build_in_process_embedder().await?;
            Ok((embedder, None, BackendKind::InProcess, false))
        }

        // ── HTTP remote (manually managed embedderd) ───────────────────────
        addr if addr.starts_with("http://") || addr.starts_with("https://") => {
            tracing::info!("embedder mode: remote http ({})", addr);
            let client = trusty_common::embedder_client::RemoteEmbedderClient::new(addr.to_owned());
            Ok((
                Arc::new(RemoteEmbedderAdapter {
                    client: EmbedderClientKind::Http(client),
                }),
                None,
                BackendKind::Remote,
                false,
            ))
        }

        // ── UDS remote (manually managed embedderd) ────────────────────────
        path if path.starts_with("unix:") => {
            let sock = PathBuf::from(&path["unix:".len()..]);
            tracing::info!("embedder mode: remote uds ({})", sock.display());
            let client = trusty_common::embedder_client::UdsEmbedderClient::new(sock);
            Ok((
                Arc::new(UdsEmbedderAdapter { client }),
                None,
                BackendKind::Remote,
                false,
            ))
        }

        other => anyhow::bail!(
            "invalid TRUSTY_EMBEDDER value: {other:?}. \
             Expected: unset (default stdio sidecar), 'auto', 'stdio', 'python' \
             (opt-in Python/MPS sidecar), 'in-process', \
             'http://...', or 'unix:/path/to/socket'"
        ),
    }
}

/// Eagerly bootstrap the python venv and arm a lazy-spawn `LazyEmbedderHandle`
/// for the python/MPS sidecar, falling back to the ort stdio sidecar on ANY
/// bootstrap or launcher-discovery failure so search never hard-fails.
///
/// Why: extracted from the former inline `"python"` match arm body (epic
/// #3524 slices 2-4) so the exact same eager-blocking logic is reachable both
/// from the explicit `TRUSTY_EMBEDDER=python` value AND from unset/`auto` +
/// `TRUSTY_EMBEDDER_PYTHON_EAGER` (PR 3/5) without duplicating it.
/// What: blocking `ensure_venv_eager` off the async runtime, then
/// `locate_launcher_binary`, then arms a `LazyEmbedderHandle` exactly like
/// `build_ort_stdio_sidecar` does for the ort binary.
/// Test: unaffected by this extraction — same behavior, same call path,
/// covered by the existing `lazy_adapter_python_reports_mps_provider`.
async fn build_eager_python_embedder() -> Result<(
    std::sync::Arc<dyn crate::core::Embedder>,
    Option<Arc<AtomicU32>>,
    BackendKind,
)> {
    use crate::service::embedder_supervisor::LazyEmbedderHandle;

    // The bootstrap is a blocking, potentially minutes-long one-time
    // build; run it off the async runtime. `ensure_venv_eager` (not the
    // plain `ensure_venv` the per-respawn launcher binary uses) pays for
    // the FULL torch-importing `.ready` recheck — worth it here since
    // this call happens once per daemon lifetime, not once per respawn.
    let bootstrap = tokio::task::spawn_blocking(trusty_embedderd_py::ensure_venv_eager)
        .await
        .map_err(|e| anyhow::anyhow!("py-embedder bootstrap task panicked: {e}"));

    match bootstrap.and_then(|r| r.map_err(|e| e.context("py-embedder venv bootstrap"))) {
        Ok(_layout) => match trusty_embedderd_py::locate_launcher_binary() {
            Ok(launcher) => {
                let config = resolve_python_supervisor_config();
                tracing::info!(
                    "embedder mode: python/MPS sidecar lazy \
                     (launcher={}, idle_shutdown_secs={})",
                    launcher.display(),
                    config.idle_shutdown_secs,
                );
                // Slice 5 (epic #3524): the runtime fallback trips on the
                // supervisor's OWN give-up signal
                // (`Embedder::supervisor_gave_up`, forwarded from
                // `LazyEmbedderHandle::supervisor_gave_up` →
                // `SupervisorHandle::has_given_up`) rather than an
                // independently-counted request-failure threshold — see
                // `embedder_fallback.rs`'s module doc (review finding, PR
                // #3560 HIGH fix) for the full rationale.
                let handle = Arc::new(LazyEmbedderHandle::new(launcher, config));
                let pid_slot = handle.app_pid_slot();
                let python_embedder: Arc<dyn crate::core::Embedder> =
                    Arc::new(LazySlotEmbedderAdapter {
                        handle: Arc::clone(&handle),
                        is_python: true,
                    });
                let fallback_adapter = super::embedder_fallback::FallbackEmbedderAdapter::new(
                    python_embedder,
                    || build_ort_stdio_sidecar().map(|(embedder, _pid_slot)| embedder),
                );
                Ok((Arc::new(fallback_adapter), Some(pid_slot), BackendKind::Python))
            }
            Err(e) => {
                tracing::warn!(
                    "TRUSTY_EMBEDDER=python: could not locate the \
                     trusty-embedderd-py launcher ({e:#}) — FALLING BACK to the \
                     Rust ort stdio sidecar. Set TRUSTY_EMBEDDERD_PY_BIN or ensure \
                     the launcher is on PATH to use the Python/MPS sidecar."
                );
                build_ort_stdio_sidecar().map(|(e, p)| (e, p, BackendKind::Ort))
            }
        },
        Err(e) => {
            tracing::warn!(
                "TRUSTY_EMBEDDER=python: venv bootstrap failed ({e:#}) — FALLING \
                 BACK to the Rust ort stdio sidecar so search does not hard-fail. \
                 Ensure `uv` is installed (or set TRUSTY_UV_BIN) and there is \
                 ~3 GB free disk, then restart to retry the Python/MPS sidecar."
            );
            build_ort_stdio_sidecar().map(|(e, p)| (e, p, BackendKind::Ort))
        }
    }
}

/// Python-arm-only override of the shared 300s idle-shutdown default (fast-follow,
/// epic #3524).
///
/// Why: the shared `TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS` default (300s / 5 min,
/// issue #2315) is tuned for the lightweight Rust ort sidecar, whose cold
/// restart is cheap. The Python/MPS sidecar's cold restart (torch import +
/// model load + one MPS warmup) is only ~2.5-3s, but reclaiming it every 5
/// minutes of think-time means a normal ~30 minute work session (edit, read,
/// edit again) repeatedly pays that cost for no real memory benefit in
/// between. Raising the DEFAULT to 1800s (30 min) — for the python arm only —
/// keeps the sidecar warm through a typical session while still reclaiming
/// its ~500 MB after genuine extended idle, which matters on the 16 GB
/// minimum-spec tier. Operators on higher-RAM machines can set
/// `TRUSTY_EMBEDDERD_PY_IDLE_SHUTDOWN_SECS=0` for always-warm (idle-shutdown
/// disabled).
///
/// What: resolution precedence (see [`resolve_python_idle_shutdown_secs`] for
/// the pure logic): `TRUSTY_EMBEDDERD_PY_IDLE_SHUTDOWN_SECS`, if set to a
/// valid `u64` (including `0`), wins outright. Otherwise, if the operator
/// explicitly set the SHARED `TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS` (present in
/// the environment at all — any value, including `0`), that value is
/// honoured: their intent must not be silently overridden. Otherwise the
/// shared 300s `SupervisorConfig::from_env()` default is replaced with 1800
/// for this arm only. The ort/default arm (`build_ort_stdio_sidecar`) still
/// calls `SupervisorConfig::from_env()` directly and is completely unaffected.
/// Test: `resolve_python_idle_shutdown_secs_*` in `start/tests.rs`.
pub(super) fn resolve_python_supervisor_config(
) -> crate::service::embedder_supervisor::SupervisorConfig {
    use crate::service::embedder_supervisor::SupervisorConfig;

    let mut config = SupervisorConfig::from_env();
    config.idle_shutdown_secs = resolve_python_idle_shutdown_secs(
        std::env::var("TRUSTY_EMBEDDERD_PY_IDLE_SHUTDOWN_SECS").ok(),
        std::env::var("TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS").ok(),
        config.idle_shutdown_secs,
    );
    config
}

/// Pure resolution logic for [`resolve_python_supervisor_config`] — unit
/// testable without touching real process env vars.
///
/// `py_var` / `shared_var` are the RAW `std::env::var(..).ok()` results (so
/// `None` means "not present at all", distinguishing "unset" from
/// "explicitly set to the same value as the default" — `SupervisorConfig`
/// alone cannot make that distinction). `shared_resolved` is the value
/// `SupervisorConfig::from_env()` already computed for the shared var
/// (300 when unset/malformed, or the operator's parsed value) — reused here
/// so a malformed shared value falls back exactly like `parse_env_u64` does,
/// without re-implementing that parsing.
pub(super) fn resolve_python_idle_shutdown_secs(
    py_var: Option<String>,
    shared_var: Option<String>,
    shared_resolved: u64,
) -> u64 {
    const PYTHON_IDLE_DEFAULT_SECS: u64 = 1800;

    if let Some(raw) = py_var {
        if let Ok(secs) = raw.trim().parse::<u64>() {
            return secs;
        }
        // Malformed py-specific value: ignore it (matches `parse_env_u64`'s
        // "malformed falls through" convention) and fall through to the
        // shared-var-or-1800 resolution below.
    }

    match shared_var {
        // Operator explicitly touched the shared var (any value, including a
        // malformed one that resolved to 300 via `parse_env_u64`) — honour
        // their intent rather than silently applying the python-specific
        // default on top of it.
        Some(_) => shared_resolved,
        None => PYTHON_IDLE_DEFAULT_SECS,
    }
}

/// Construct the default Rust ort stdio-sidecar `LazyEmbedderHandle`.
///
/// Why: this is the default `auto`/`stdio` path AND the fallback target for the
/// opt-in `TRUSTY_EMBEDDER=python` arm (epic #3524) when the Python venv
/// bootstrap or launcher discovery fails — search must never hard-fail, so it
/// degrades to the Rust ort embedder. Extracted so both call sites share one
/// implementation.
/// What: locates `trusty-embedderd` (fail-fast install hint if missing) and
/// arms a `LazyEmbedderHandle` (deferred spawn — no child at boot).
/// Test: the default-path behaviour is covered by `start/tests.rs`
/// (`lazy_adapter_reports_resolved_provider`).
/// The pair every embedder-construction path returns: the `Arc<dyn Embedder>`
/// plus the optional sidecar PID slot (`Some` only for stdio-sidecar paths).
type BuiltEmbedder = (
    std::sync::Arc<dyn crate::core::Embedder>,
    Option<Arc<AtomicU32>>,
);

fn build_ort_stdio_sidecar() -> Result<BuiltEmbedder> {
    use crate::service::embedder_supervisor::{
        locate_embedderd_binary, LazyEmbedderHandle, SupervisorConfig,
    };

    // `trusty-embedderd` is a required runtime dependency — fail fast with an
    // actionable install hint rather than silently downgrading to in-process
    // embedding. Users who need to skip the sidecar for tests or debugging must
    // set `TRUSTY_EMBEDDER=in-process` explicitly.
    let binary = locate_embedderd_binary().map_err(|e| {
        anyhow::anyhow!(
            "{e}\n\n\
             ERROR: trusty-embedderd binary not found on PATH.\n\
             \n\
             trusty-search v0.13+ requires trusty-embedderd to be installed alongside it.\n\
             \n\
             Install it with:\n\
             \x20 cargo install trusty-embedderd --locked\n\
             \n\
             Or set TRUSTY_EMBEDDERD_BIN to an absolute path:\n\
             \x20 export TRUSTY_EMBEDDERD_BIN=/path/to/trusty-embedderd\n\
             \n\
             If you need to run without the sidecar (tests, debugging), use:\n\
             \x20 TRUSTY_EMBEDDER=in-process trusty-search start"
        )
    })?;

    let config = SupervisorConfig::from_env();

    tracing::info!(
        "embedder mode: stdio-sidecar lazy (binary={}, idle_shutdown_secs={})",
        binary.display(),
        config.idle_shutdown_secs,
    );

    // Issue #315: construct the lazy handle (no child spawned yet).
    let handle = Arc::new(LazyEmbedderHandle::new(binary, config));
    let pid_slot = handle.app_pid_slot();

    Ok((
        Arc::new(LazySlotEmbedderAdapter {
            handle,
            is_python: false,
        }),
        Some(pid_slot),
    ))
}

/// Build the in-process `FastEmbedder` and log details.
///
/// Why: extracted from the monolithic `build_embedder` so the `in-process`
/// escape-hatch path has a clean, focused helper. This is never called from
/// the default `auto`/`stdio` path — it is only reachable via explicit
/// `TRUSTY_EMBEDDER=in-process` or `TRUSTY_EMBEDDER=local`.
/// What: constructs `FastEmbedder`, logs the provider / dimension, applies
/// GPU batch-size tuning, and wraps in an `Arc<dyn Embedder>`.
/// Test: exercised when `TRUSTY_EMBEDDER=in-process` is set explicitly.
async fn build_in_process_embedder() -> Result<Arc<dyn crate::core::Embedder>> {
    let embedder = crate::core::FastEmbedder::new().await.map_err(|e| {
        tracing::error!("FastEmbedder init failed: {e:#}");
        anyhow::anyhow!("FastEmbedder init failed: {e}")
    })?;
    let dim = <crate::core::FastEmbedder as crate::core::Embedder>::dimension(&embedder);
    let provider = embedder.provider();
    let metal_hint = match provider {
        trusty_common::embedder::ExecutionProvider::CoreML => " (Metal GPU + ANE + CPU)",
        trusty_common::embedder::ExecutionProvider::CoreMLAne => " (Neural Engine + CPU)",
        trusty_common::embedder::ExecutionProvider::Cuda => " (CUDA GPU)",
        trusty_common::embedder::ExecutionProvider::Mps => " (Metal Performance Shaders)",
        trusty_common::embedder::ExecutionProvider::Cpu => "",
    };
    // Issue #3530: report the RESOLVED model name (fp32 default vs the
    // explicit int8 opt-in) instead of the stale hardcoded "AllMiniLML6V2(Q)".
    let model_name = embedder.model_name();
    tracing::info!(
        "embedder initialized: model={model_name} dim={dim} provider={provider}{metal_hint}"
    );
    tune_batch_size_for_provider(provider);
    Ok(Arc::new(embedder))
}

/// Internal enum for the HTTP remote adapter to hold either HTTP or UDS client.
///
/// Why: avoids duplicating the `RemoteEmbedderAdapter` struct for the two HTTP
/// variants — both share identical adapter logic and differ only in the
/// concrete `EmbedderClient` impl they hold.
/// What: two variants, each wrapping the corresponding `trusty_common`
/// client type.
/// Test: exercised via `TRUSTY_EMBEDDER=http://...` (Http variant) startup.
enum EmbedderClientKind {
    Http(trusty_common::embedder_client::RemoteEmbedderClient),
}

/// Adapter that implements trusty-search's `Embedder` trait by delegating to
/// a `RemoteEmbedderClient` (HTTP) (issue #110 Phase 1 / Phase 2).
///
/// Why: trusty-search's internal `Embedder` trait uses `&[&str]` slices;
/// `EmbedderClient` uses `Vec<String>`. This adapter bridges the two without
/// modifying either side.
/// What: holds an `EmbedderClientKind` and impls the local `Embedder`
/// facade that `CodeIndexer` and `EmbedPool` hold behind `Arc<dyn Embedder>`.
/// Test: exercised end-to-end when `TRUSTY_EMBEDDER=http://...` is set at
/// daemon startup; the `bit_identical` integration test validates correctness.
struct RemoteEmbedderAdapter {
    client: EmbedderClientKind,
}

#[async_trait::async_trait]
impl crate::core::Embedder for RemoteEmbedderAdapter {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        use trusty_common::embedder_client::EmbedderClient as _;
        let mut v = match &self.client {
            EmbedderClientKind::Http(c) => c
                .embed_batch(vec![text.to_string()])
                .await
                .map_err(|e| anyhow::anyhow!("remote embed failed: {e}"))?,
        };
        v.pop()
            .ok_or_else(|| anyhow::anyhow!("remote embedder returned no vector"))
    }

    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        use trusty_common::embedder_client::EmbedderClient as _;
        let owned: Vec<String> = texts.iter().map(|s| (*s).to_owned()).collect();
        match &self.client {
            EmbedderClientKind::Http(c) => c
                .embed_batch(owned)
                .await
                .map_err(|e| anyhow::anyhow!("remote embed_batch failed: {e}")),
        }
    }

    fn dimension(&self) -> usize {
        trusty_common::embedder::EMBED_DIM
    }

    /// Report the execution provider the remote `trusty-embedderd` resolves.
    ///
    /// Why: issue #604. The remote sidecar selects its EP through this crate's
    /// `init_options`, which is a pure function of build features + env, so the
    /// parent can predict the same answer and `/health` reports the real
    /// provider instead of the trait-default `CPU`.
    /// What: delegates to `trusty_common::embedder::resolve_expected_provider`.
    /// Test: `resolve_expected_provider_*` in trusty-common cover the resolver;
    /// real-GPU end-to-end is hardware-gated.
    fn provider(&self) -> trusty_common::embedder::ExecutionProvider {
        trusty_common::embedder::resolve_expected_provider()
    }
}

/// Adapter that implements trusty-search's `Embedder` trait by delegating to
/// a `UdsEmbedderClient` (issue #110 Phase 2).
///
/// Why: the auto-spawn path uses a UDS socket for low-latency IPC; this
/// adapter bridges the `EmbedderClient` trait (Vec<String>) to the internal
/// `Embedder` trait (&[&str]) without changing either side.
/// What: holds a `UdsEmbedderClient` and delegates `embed` / `embed_batch`
/// calls through the EmbedderClient trait.
/// Test: exercised whenever `TRUSTY_EMBEDDER` is unset (auto-spawn) or set to
/// `unix:/path`; the `supervisor_spawns_and_serves_embed_requests` integration
/// test validates round-trip correctness.
pub(super) struct UdsEmbedderAdapter {
    pub(super) client: trusty_common::embedder_client::UdsEmbedderClient,
}

#[async_trait::async_trait]
impl crate::core::Embedder for UdsEmbedderAdapter {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        use trusty_common::embedder_client::EmbedderClient as _;
        let mut v = self
            .client
            .embed_batch(vec![text.to_string()])
            .await
            .map_err(|e| anyhow::anyhow!("uds embed failed: {e}"))?;
        v.pop()
            .ok_or_else(|| anyhow::anyhow!("uds embedder returned no vector"))
    }

    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        use trusty_common::embedder_client::EmbedderClient as _;
        let owned: Vec<String> = texts.iter().map(|s| (*s).to_owned()).collect();
        self.client
            .embed_batch(owned)
            .await
            .map_err(|e| anyhow::anyhow!("uds embed_batch failed: {e}"))
    }

    fn dimension(&self) -> usize {
        trusty_common::embedder::EMBED_DIM
    }

    /// Report the execution provider the UDS-remote `trusty-embedderd` resolves.
    ///
    /// Why: issue #604 — see `RemoteEmbedderAdapter::provider`. The UDS sidecar
    /// runs the same `init_options` resolution, so the parent predicts the same
    /// provider for `/health`.
    /// What: delegates to `trusty_common::embedder::resolve_expected_provider`.
    /// Test: covered by trusty-common's `resolve_expected_provider_*` tests.
    fn provider(&self) -> trusty_common::embedder::ExecutionProvider {
        trusty_common::embedder::resolve_expected_provider()
    }
}

/// Adapter for the lazy stdio sidecar path (issue #315).
///
/// Why: `LazyEmbedderHandle` defers the child spawn to the first embed call.
/// This adapter satisfies the `Arc<dyn Embedder>` interface that the rest of
/// the daemon holds, forwarding every `embed` / `embed_batch` call through
/// `LazyEmbedderHandle::embed_via` which triggers the spawn on first use and
/// then routes through the supervisor's slot on all subsequent calls.
/// What: holds an `Arc<LazyEmbedderHandle>` and delegates both `embed` and
/// `embed_batch` through `embed_via`. The lazy handle handles single-flight
/// spawn, crash-restart transparency, and optional idle-shutdown internally.
/// Test: exercised whenever `TRUSTY_EMBEDDER` is unset or set to `auto` /
/// `stdio`. The `supervisor_spawns_and_serves_embed_requests` integration test
/// validates round-trip correctness (marked `#[ignore]`, requires binary).
pub(super) struct LazySlotEmbedderAdapter {
    pub(super) handle: Arc<crate::service::embedder_supervisor::LazyEmbedderHandle>,
    /// Which provider-prediction resolver `provider()` must delegate to
    /// (epic #3524 slice 6 / issue #3493 P1).
    ///
    /// Why: this SAME adapter type wraps both the default Rust ort stdio
    /// sidecar (`trusty-embedderd`) and the opt-in Python/MPS sidecar
    /// (`trusty-embedderd-py`) — they speak the identical JSON-RPC 2.0 stdio
    /// protocol, so `LazyEmbedderHandle` cannot tell them apart. But the two
    /// sidecars select their execution provider through entirely different
    /// code (ONNX Runtime's CoreML/CUDA/CPU EPs vs. torch's MPS/CUDA/CPU
    /// device), so `provider()` must predict through the matching resolver —
    /// calling the ORT-oriented `resolve_expected_provider` for the Python
    /// arm is exactly the pre-fix bug (`/health` reported `CoreML` for a
    /// sidecar that never touches ONNX Runtime at all).
    /// What: `false` for the ort stdio sidecar (`build_ort_stdio_sidecar`),
    /// `true` for the python arm (`build_embedder_raw`'s `"python"` match
    /// arm's success path only — its own ort-fallback sub-branches
    /// construct a separate adapter with `is_python: false`).
    /// Test: `lazy_adapter_reports_resolved_provider` (ort),
    /// `lazy_adapter_python_reports_mps_provider` (python) in `start/tests.rs`.
    pub(super) is_python: bool,
}

#[async_trait::async_trait]
impl crate::core::Embedder for LazySlotEmbedderAdapter {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let text_owned = text.to_string();
        let mut v = self
            .handle
            .embed_via(|client| async move { client.embed_batch(vec![text_owned]).await })
            .await
            .map_err(|e| anyhow::anyhow!("lazy-stdio embed failed: {e}"))?;
        v.pop()
            .ok_or_else(|| anyhow::anyhow!("lazy-stdio embedder returned no vector"))
    }

    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let owned: Vec<String> = texts.iter().map(|s| (*s).to_owned()).collect();
        self.handle
            .embed_via(|client| async move { client.embed_batch(owned).await })
            .await
            .map_err(|e| anyhow::anyhow!("lazy-stdio embed_batch failed: {e}"))
    }

    fn dimension(&self) -> usize {
        trusty_common::embedder::EMBED_DIM
    }

    /// Report the execution provider the lazy stdio sidecar resolves.
    ///
    /// Why: issue #604 — this is the **default** deployment path, and it was the
    /// direct cause of `/health` reporting `provider=CPU` while the sidecar's
    /// own startup log said `provider=CUDA`. The `LazyEmbedderHandle` defers the
    /// child spawn, so there is no live provider to read until the first embed;
    /// rather than report a stale `CPU`, predict the provider the sidecar will
    /// resolve. This is correct even before the child has spawned, because the
    /// resolution is a pure function of build features + env.
    ///
    /// Issue #3493 P1: this adapter also wraps the Python/MPS sidecar (see
    /// `is_python`'s doc), whose device selection is torch's, not ONNX
    /// Runtime's — delegating unconditionally to the ORT-oriented resolver
    /// reported `CoreML` for a sidecar that never runs CoreML at all.
    /// What: delegates to `trusty_common::embedder::resolve_expected_python_provider`
    /// when `is_python` is set, otherwise `resolve_expected_provider`.
    /// Test: covered by trusty-common's `resolve_expected_provider_*` /
    /// `resolve_expected_python_provider_*` tests; real-GPU/MPS validation is
    /// hardware-gated.
    fn provider(&self) -> trusty_common::embedder::ExecutionProvider {
        if self.is_python {
            trusty_common::embedder::resolve_expected_python_provider()
        } else {
            trusty_common::embedder::resolve_expected_provider()
        }
    }

    /// Real-readback override (epic #3524 slice 5, issue #3493 P1).
    ///
    /// Why: `provider()` above PREDICTS via `resolve_expected_provider()` — a
    /// pure function of build features/env with no notion of "this handle is
    /// actually running the Python/MPS sidecar via torch". For the default
    /// Rust ort sidecar that prediction is the best available answer (no
    /// wire readback exists on that transport), but this same adapter type
    /// is ALSO used for the opt-in `TRUSTY_EMBEDDER=python` arm, where it
    /// actively lies: predicting the ORT-flavoured `CoreML(ANE)` guess while
    /// torch actually selected `mps` (the exact #3493 P1 bug).
    /// What: forwards to `LazyEmbedderHandle::last_reported_device()` — the
    /// non-blocking readback of the live client's wire-reported device.
    /// `None` before the first successful embed (nothing reported yet) or on
    /// the Rust ort transport (which never sends the field), in which case
    /// `/health` falls back to `provider()`'s prediction.
    /// Test: `lazy_python_adapter_reports_wire_device` in `start/tests.rs`.
    fn resolved_provider_label(&self) -> Option<String> {
        self.handle.last_reported_device()
    }

    /// Real give-up-signal override (review finding, PR #3560 HIGH fix).
    ///
    /// Why: `FallbackEmbedderAdapter` needs the SAME supervisor's actual
    /// give-up decision, not a proxy reconstructed at the request layer —
    /// see `Embedder::supervisor_gave_up`'s trait-default doc comment for the
    /// full rationale.
    /// What: forwards to `LazyEmbedderHandle::supervisor_gave_up()`.
    /// Test: `lazy_python_adapter_reports_supervisor_gave_up_default_false`
    /// in `start/tests.rs`.
    fn supervisor_gave_up(&self) -> bool {
        self.handle.supervisor_gave_up()
    }
}

/// When the resolved execution provider is a GPU, retune `TRUSTY_MAX_BATCH_SIZE`
/// upward so ONNX dispatches use the GPU efficiently.
///
/// Why (issue #113): the CPU batch-size formula (≈55 MB transient ORT arena
/// per batch slot, clamped to `[32, 512]`) is sized to keep the *CPU* ORT
/// path under the soft RSS cap. On a CUDA GPU the arena lives in device
/// memory and the per-slot transient is much smaller, so the CPU-tuned
/// default (e.g. 128 on Medium tier) starves the GPU — most wall-clock is
/// spent on host↔device round-trips instead of compute. Bumping the default
/// to 512 cuts host↔device transitions ~4× and is what turns a 5-hour
/// reindex into a ~30-minute one on a Tesla T4 (40 k files, INT8 model).
/// What: if a GPU EP is active AND the operator did NOT opt out by setting
/// `TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1`, retune `TRUSTY_MAX_BATCH_SIZE` to 512.
/// Test: on a CUDA-enabled binary the startup log shows
/// `gpu_batch_tuning: TRUSTY_MAX_BATCH_SIZE=512 (was N)`; running with
/// `TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1 TRUSTY_MAX_BATCH_SIZE=256 trusty-search start`
/// keeps 256.
pub(super) fn tune_batch_size_for_provider(provider: trusty_common::embedder::ExecutionProvider) {
    const GPU_BATCH_DEFAULT: usize = 512;

    // CoreML is intentionally excluded from the GPU batch-size bump.
    //
    // Why: unlike CUDA (whose ORT arena lives in device memory), CoreML on
    // Apple Silicon pre-allocates GPU/ANE buffers in the *unified* memory
    // pool and those buffers stack between calls. Bumping TRUSTY_MAX_BATCH_SIZE
    // to 512 on CoreML reliably inflates process RSS by ~70 GB in seconds
    // and triggers macOS jetsam SIGKILL. The reindex pipeline now uses
    // `TRUSTY_COREML_BATCH_SIZE` (default 32) when CoreML is active —
    // see `core::indexer::ingest::embed_chunks_in_batches`. Leaving
    // `TRUSTY_MAX_BATCH_SIZE` at its tier default is the safe answer.
    if matches!(
        provider,
        trusty_common::embedder::ExecutionProvider::CoreML
            | trusty_common::embedder::ExecutionProvider::CoreMLAne
    ) {
        let coreml_bs = crate::core::resolve_coreml_batch_size();
        tracing::info!(
            "gpu_batch_tuning: provider={provider} → using TRUSTY_COREML_BATCH_SIZE={coreml_bs} for \
             indexing batches (CoreML EP allocates per-batch buffers in the unified-memory pool)"
        );
        return;
    }

    let is_gpu = matches!(provider, trusty_common::embedder::ExecutionProvider::Cuda);
    if !is_gpu {
        return;
    }

    if std::env::var("TRUSTY_MAX_BATCH_SIZE_EXPLICIT")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        tracing::info!(
            "gpu_batch_tuning: TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1 set, leaving batch size unchanged"
        );
        return;
    }

    let current = std::env::var("TRUSTY_MAX_BATCH_SIZE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(128);
    if current >= GPU_BATCH_DEFAULT {
        return;
    }

    // SAFETY: invoked on the main thread before any indexing worker has
    // started. Same invariant `MemoryPolicy::apply_to_env` relies on.
    unsafe {
        std::env::set_var("TRUSTY_MAX_BATCH_SIZE", GPU_BATCH_DEFAULT.to_string());
    }
    tracing::info!(
        "gpu_batch_tuning: provider={provider} → TRUSTY_MAX_BATCH_SIZE={GPU_BATCH_DEFAULT} (was {current}); \
         set TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1 to keep your value"
    );
}
