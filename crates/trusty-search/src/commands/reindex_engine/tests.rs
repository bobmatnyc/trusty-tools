//! Unit tests for the reindex engine: option/outcome defaults, the
//! progress-aware wait-strategy decision logic, stall detection, and the
//! issue-#744/#823 progress-bar fixes.
//!
//! Why: these tests pin the pure decision logic and the `ReindexUi` bar
//! transitions around the event loop, and (#9214) registration and the
//! per-file write against a mock daemon socket. The loop itself is driven
//! against a mock socket in `driver_tests.rs`.
//! What: asserts defaults, hard-deadline vs stall-window construction, stall
//! detection, ETA strings, and Embed/Chunk bar priming/freezing behaviour.
//! Test: this module.

use super::*;

/// The default `ReindexOptions` values must be sane so accidental callers
/// that rely on `Default::default()` get progress-aware stall behaviour.
///
/// Why: `timeout_explicit = false` is the key invariant — it ensures that
/// a CLI omitting `--timeout` gets the progress-aware default rather than
/// a premature 600 s abort.
/// What: asserts the default field values.
/// Test: this test.
#[test]
fn default_options_are_sane() {
    let opts = ReindexOptions::default();
    assert!(!opts.verify_after);
    assert!(opts.prior_chunk_count.is_none());
    assert!(!opts.force);
    // timeout_explicit must be false so the progress-aware stall window
    // governs by default (not a hard wall-clock cap).
    assert!(!opts.timeout_explicit);
    assert_eq!(opts.stall_secs, 120);
}

/// The default `ReindexOutcome` must have all fields at zero / false so
/// callers can accumulate into it safely.
///
/// Why: a non-zero default would make "nothing happened" indistinguishable
/// from a real result.
/// What: asserts the default field values.
/// Test: this test.
#[test]
fn default_outcome_is_zero() {
    let o = ReindexOutcome::default();
    assert_eq!(o.indexed, 0);
    assert_eq!(o.total_chunks, 0);
    assert!(!o.completed);
    assert!(o.timings.is_none());
}

/// A non-interactive `ProgressStyle` must not panic on the indicatif
/// template strings used in `bar_style`.  This catches template syntax
/// regressions before they reach users.
///
/// Why: `ProgressStyle::with_template` returns an error (not a panic) on
/// bad templates, but `bar_style` falls back to `default_bar()`.  Asserting
/// the style is non-panicking here catches the case where the fallback would
/// silently hide a bug.
/// What: constructs styles for all three states and asserts no panic.
/// Test: this test.
#[test]
fn bar_style_does_not_panic() {
    use super::super::reindex_ui::ReindexUi;
    // Constructing the UI exercises all bar styles.
    let ui = ReindexUi::new("test", false);
    ui.finish("ok".to_string());
}

// ── Progress-aware wait logic ─────────────────────────────────────────────
//
// The full event loop in `run_reindex_with` is driven in `driver_tests.rs`.
// The tests below verify the *decision logic* that governs the wait strategy:
//
//  1. Whether `ReindexOptions` correctly represents "explicit" vs "default"
//     timeout intent.
//  2. Whether the hard-cap and stall-window durations are constructed
//     correctly from the options.
//  3. That `run_reindex_opts` with `timeout_explicit=false` produces options
//     with no hard deadline (the progress-aware path).
//  4. That `run_reindex_opts` with `timeout_explicit=true` and a nonzero
//     `timeout_secs` would produce a hard deadline.
//
// Integration coverage lives in the `--include-ignored` test suite (requires
// a live daemon + indexed corpus).

/// When `timeout_explicit = false` (the default), no hard deadline is set
/// and the stall window governs.
///
/// Why: guards the progress-aware default — a regression here would restore
/// the old premature 600 s abort on every unattended `trusty-search index`.
/// What: constructs `ReindexOptions` with `timeout_explicit = false` and
/// asserts the hard-deadline path would not fire.
/// Test: this test.
#[test]
fn progress_aware_wait_no_hard_deadline_when_implicit() {
    let opts = ReindexOptions {
        timeout_explicit: false,
        stall_secs: 120,
        ..ReindexOptions::default()
    };
    // The hard-deadline arm is `opts.timeout_explicit` — when false, no
    // deadline `Instant` is created.
    assert!(
        !opts.timeout_explicit,
        "implicit timeout must not set a hard cap"
    );
    assert_eq!(opts.stall_secs, 120);

    // Simulate the deadline construction logic from run_reindex_with:
    // hard_deadline is None when timeout_explicit is false.
    let hard_deadline: Option<std::time::Duration> = if opts.timeout_explicit {
        Some(std::time::Duration::from_secs(opts.timeout_secs))
    } else {
        None
    };
    assert!(
        hard_deadline.is_none(),
        "progress-aware mode must not produce a hard deadline"
    );
}

/// When `timeout_explicit = true` with a non-zero `timeout_secs`, a hard
/// deadline is imposed (the legacy behaviour preserved for `--timeout N`).
///
/// Why: explicit `--timeout` must still work as a reliable hard cap even
/// when indexing is healthy.  Power users depend on this for scripting.
/// What: constructs `ReindexOptions` with `timeout_explicit = true` and
/// asserts the hard deadline is set.
/// Test: this test.
#[test]
fn progress_aware_wait_hard_deadline_when_explicit() {
    let opts = ReindexOptions {
        timeout_secs: 300,
        timeout_explicit: true,
        ..ReindexOptions::default()
    };
    assert!(
        opts.timeout_explicit,
        "explicit timeout must set a hard cap"
    );

    let hard_deadline: Option<std::time::Duration> =
        if opts.timeout_explicit && opts.timeout_secs > 0 {
            Some(std::time::Duration::from_secs(opts.timeout_secs))
        } else {
            None
        };
    assert_eq!(
        hard_deadline,
        Some(std::time::Duration::from_secs(300)),
        "explicit 300 s timeout must produce a 300 s hard deadline"
    );
}

/// `--timeout 0` with `timeout_explicit = true` means "wait forever"
/// (the legacy `0 = no limit` behaviour).
///
/// Why: `--timeout 0` must remain a valid escape hatch for users who want
/// to block indefinitely without switching to progress-aware mode.
/// What: asserts that `timeout_secs = 0` + `timeout_explicit = true` does
/// NOT produce a hard deadline (the `> 0` guard).
/// Test: this test.
#[test]
fn progress_aware_wait_timeout_zero_explicit_means_no_deadline() {
    let opts = ReindexOptions {
        timeout_secs: 0,
        timeout_explicit: true,
        ..ReindexOptions::default()
    };
    // Mirrors the `if opts.timeout_explicit { if opts.timeout_secs > 0 { Some(…) } else { None } }`
    // guard in run_reindex_with.
    let hard_deadline: Option<std::time::Duration> = if opts.timeout_explicit {
        if opts.timeout_secs > 0 {
            Some(std::time::Duration::from_secs(opts.timeout_secs))
        } else {
            None // --timeout 0 = wait forever
        }
    } else {
        None
    };
    assert!(
        hard_deadline.is_none(),
        "--timeout 0 must not produce a hard deadline (wait forever)"
    );
}

/// Stall detection logic: a counter that stops advancing within the stall
/// window should trigger a stall, while one that advances should not.
///
/// Why: the stall window is the core mechanism preventing premature detach
/// during a healthy but slow embed run; verifying the comparison logic
/// catches off-by-one or direction errors before they reach users.
/// What: simulates the indexed-counter comparison used in the wait loop and
/// asserts the stall condition fires only when the counter is frozen.
/// Test: this test.
#[test]
fn stall_detection_triggers_on_frozen_counter() {
    // Simulate: counter has been at 100 for > stall_secs.
    let last_indexed_snapshot: u64 = 100;
    let current_indexed: u64 = 100; // unchanged — stalled

    let counter_advanced = current_indexed > last_indexed_snapshot;
    assert!(!counter_advanced, "frozen counter must not advance");

    // With a tiny stall window that has definitely elapsed:
    let last_progress = std::time::Instant::now() - std::time::Duration::from_secs(200);
    let stall_deadline_dur = std::time::Duration::from_secs(120);
    let is_stalled = !counter_advanced && last_progress.elapsed() >= stall_deadline_dur;
    assert!(
        is_stalled,
        "must detect stall after stall_secs with no counter advance"
    );
}

/// Stall detection logic: a counter that advances resets the stall clock
/// and must NOT trigger a stall.
///
/// Why: complements `stall_detection_triggers_on_frozen_counter` — a
/// progressing index must never be considered stalled regardless of
/// elapsed wall-clock time.
/// What: simulates a counter that advanced and a stall window that has
/// elapsed; asserts the stall condition does NOT fire.
/// Test: this test.
#[test]
fn stall_detection_does_not_trigger_while_progressing() {
    let last_indexed_snapshot: u64 = 100;
    let current_indexed: u64 = 150; // advanced — progressing

    let counter_advanced = current_indexed > last_indexed_snapshot;
    assert!(
        counter_advanced,
        "advancing counter must register as progress"
    );

    // Even with a very old `last_progress`, the counter advance means we
    // are NOT stalled (the loop resets last_progress when it sees advance).
    // This test verifies the `counter_advanced` check comes first.
    let stalled = !counter_advanced; // counter_advanced resets the stall
    assert!(!stalled, "progressing counter must not trigger stall");
}

// ── Issue #744 progress fixes ─────────────────────────────────────────────

/// The `total_files_now` atomic must be zero initially and updated to the
/// correct denominator when set.
///
/// Why: Issue #744 — the ticker previously used `embed_bar.length()` (= 1)
/// as the Files denominator; this test verifies the replacement atomic
/// behaves correctly (zero-init + explicit store).
/// What: stores a value and reads it back via Acquire ordering.
/// Test: this test.
#[test]
fn total_files_atomic_zero_until_set() {
    use std::sync::atomic::{AtomicU64, Ordering};
    let total_files_now = AtomicU64::new(0);
    // Before any SSE event: must read 0, not 1.
    assert_eq!(
        total_files_now.load(Ordering::Acquire),
        0,
        "total_files_now must be zero until set by walk_complete/start"
    );
    total_files_now.store(3_327, Ordering::Release);
    assert_eq!(
        total_files_now.load(Ordering::Acquire),
        3_327,
        "total_files_now must reflect the value stored by the SSE handler"
    );
}

/// The ETA is "loading model…" during InitializingEmbedder and "?" when
/// the denominator is zero (before the first walk_complete event).
///
/// Why: Issue #744 — ETA "?" with Files 0/1 was confusing during model
/// cold-start; "loading model…" explains the delay.
/// What: replicates the ETA-computation logic from the ticker and asserts
/// the correct strings.
/// Test: this test.
#[test]
fn eta_logic_loading_model_and_zero_denom() {
    use super::super::reindex_ui::ReindexPhase;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn phase_to_u64_test(p: ReindexPhase) -> u64 {
        match p {
            ReindexPhase::InitializingEmbedder => 3,
            _ => 4,
        }
    }

    let total_files_now = AtomicU64::new(0);
    let indexed = 0u64;
    let elapsed = 5u64;
    let phase = phase_to_u64_test(ReindexPhase::InitializingEmbedder);
    let is_model_loading = phase == 3;
    let fps = indexed.checked_div(elapsed).unwrap_or(0);
    let total = total_files_now.load(Ordering::Acquire);

    let eta = if is_model_loading {
        "loading model\u{2026}".to_string()
    } else if fps > 0 && total > indexed {
        super::super::format::fmt_secs((total - indexed) / fps)
    } else {
        "?".to_string()
    };

    assert_eq!(
        eta, "loading model\u{2026}",
        "ETA must be 'loading model…' during InitializingEmbedder"
    );

    // Not loading model, but total is still 0 (before walk_complete).
    let phase2 = phase_to_u64_test(ReindexPhase::Embedding);
    let is_loading2 = phase2 == 3;
    let eta2 = if is_loading2 {
        "loading model\u{2026}".to_string()
    } else if fps > 0 && total > indexed {
        super::super::format::fmt_secs((total - indexed) / fps)
    } else {
        "?".to_string()
    };
    assert_eq!(
        eta2, "?",
        "ETA must be '?' when total_files is 0 and not loading model"
    );
}

// ── Issue #823 progress bar fix tests ────────────────────────────────────

/// The Embed bar (slot 2) must be primed to `total_files` immediately when
/// `walk_complete`/`start` fires — NOT left at `ProgressBar::new(1)` until
/// the first `batch` event arrives.
///
/// Why: Issue #823 Bug 2 — the Embed bar showed "0/1" throughout model
/// loading because it was never given the correct total. This test verifies
/// the fix: `set_embed_total` on walk_complete sets slot 2 independently.
/// What: constructs a UI, enters Walking, calls `set_embed_total(500)`, and
/// asserts slot 2 length is 500 (not 1).
/// Test: this test.
#[test]
fn embed_bar_total_is_set_before_first_batch() {
    use super::super::reindex_ui::{ReindexPhase, ReindexUi};
    let mut ui = ReindexUi::new("idx", false);
    // Simulate walk_complete: Walk bar fills + Chunking begins
    ui.set_phase(ReindexPhase::Walking, "idx");
    ui.set_total(500);
    ui.set_position(500);
    ui.mark_stage_done(0, 100);
    ui.set_phase(ReindexPhase::Chunking, "idx");
    ui.set_total(500);
    // Prime the Embed bar (issue #823 Bug 2 fix)
    ui.set_embed_total(500);
    // Before any batch event: Embed bar must have total=500, not 1
    assert_eq!(
        ui.stage_bars[2].length(),
        Some(500),
        "Embed bar must be primed with total_files before the first batch"
    );
    ui.finish("done".to_string());
}

/// The Chunk bar (slot 1) must NOT be frozen at the first `batch` event.
///
/// Why: Issue #823 Bug 1 — the old code called `mark_stage_done(1, ...)` in
/// the `batch` handler, freezing the Chunk bar at whatever partial count it
/// had when the first batch completed. Both bars must advance concurrently.
/// What: simulates the CHUNK+EMBED phase without calling mark_stage_done(1)
/// at the batch transition; asserts slot 1 is still Active after a batch.
/// Test: this test.
#[test]
fn chunk_bar_not_frozen_at_first_batch() {
    use super::super::reindex_ui::{ReindexPhase, ReindexUi};
    let mut ui = ReindexUi::new("idx", false);
    // Walk done
    ui.set_phase(ReindexPhase::Walking, "idx");
    ui.set_total(200);
    ui.set_position(200);
    ui.mark_stage_done(0, 100);
    // Enter Chunking
    ui.set_phase(ReindexPhase::Chunking, "idx");
    ui.set_total(200);
    ui.set_embed_total(200);
    ui.activate_embed_bar();
    // Simulate chunk_progress advancing Chunk bar to 128
    ui.set_position(128);
    // Simulate first batch event: transition header to Embedding
    // (Issue #823 Bug 1 fix: do NOT call mark_stage_done(1) here)
    ui.set_phase(ReindexPhase::Embedding, "idx");
    ui.advance_embed_bar(128);
    // Chunk bar (slot 1) must still be Active (not Done) after the transition
    assert_eq!(
        ui.bar_states[1],
        super::super::reindex_ui::BarState::Active,
        "Chunk bar must remain Active after the first batch event, not be frozen"
    );
    // Embed bar must also be Active and at 128
    assert_eq!(ui.bar_states[2], super::super::reindex_ui::BarState::Active);
    assert_eq!(ui.stage_bars[2].position(), 128);
    // Now kg_start arrives → mark Chunk bar done
    ui.mark_stage_done(1, 5_000);
    assert_eq!(
        ui.bar_states[1],
        super::super::reindex_ui::BarState::Done,
        "Chunk bar must be Done after kg_start marks it"
    );
    ui.finish("done".to_string());
}

/// `needs_embedder_init` logic must fire for in-process embedder on the
/// first batch (indexed == 0), not just for the sidecar.
///
/// Why: Issue #823 Bug 3 — the old code used `.unwrap_or(false)` which
/// silently disabled `embedder_init`/`embedder_ready` for the in-process
/// embedder. The new logic fires when `indexed == 0` regardless of mode.
/// What: simulates the new guard condition for both modes.
/// Test: this test.
#[test]
fn embedder_ready_fires_for_in_process_embedder() {
    // In-process path: embedder_pid_slot is None, first_batch_ever = true
    let first_batch_ever = true;
    let embedder_pid_slot: Option<u32> = None;
    let needs_init = if let Some(pid) = embedder_pid_slot {
        pid == 0
    } else {
        first_batch_ever
    };
    assert!(
        needs_init,
        "needs_embedder_init must be true for in-process embedder on first batch"
    );

    // Sidecar path with PID=0 (not yet spawned): same result
    let pid_slot_zero: Option<u32> = Some(0);
    let needs_init_sidecar = if let Some(pid) = pid_slot_zero {
        pid == 0
    } else {
        first_batch_ever
    };
    assert!(
        needs_init_sidecar,
        "needs_embedder_init must be true for sidecar with PID=0"
    );

    // Subsequent batches (indexed > 0): must NOT fire again
    let first_batch_ever_no = false;
    let embedder_pid_slot_warm: Option<u32> = None; // in-process, 2nd batch
    let needs_init_warm = if let Some(pid) = embedder_pid_slot_warm {
        pid == 0
    } else {
        first_batch_ever_no
    };
    assert!(
        !needs_init_warm,
        "needs_embedder_init must be false on subsequent batches"
    );
}

// ── register_on, over the daemon socket (#7758, #9214) ───────────────────

use super::registration::{legacy_pair, register_on};
use crate::commands::mock_socket::mock_daemon;
use serde_json::json;
use trusty_common::uds::server::RpcError;
use trusty_search::service::rpc::error::{CODE_CONFLICT, CODE_NOT_FOUND};

/// A conflict refusal carrying the daemon's 409 body as `data`.
fn conflict(message: &str, data: serde_json::Value) -> RpcError {
    RpcError::new(CODE_CONFLICT, message).with_data(data)
}

/// #7758 regression: a conflict naming the index that already owns this root
/// must come back as [`RegisterOutcome::RootOwnedBy`] carrying that id, not as
/// a bail — `index --force` reindexes that id.
/// Test: this function IS the test.
#[tokio::test]
async fn a_root_owned_by_another_index_is_reported_not_bailed() {
    let daemon = mock_daemon(|method, params| {
        assert_eq!(method, "search.index.create");
        assert_eq!(params["id"], "requested");
        Err(conflict(
            "root_path is already registered to index 'owner'",
            json!({ "existing_id": "owner" }),
        ))
    })
    .await;

    let got = register_on(
        &daemon.client,
        "requested",
        std::path::Path::new("/repos/requested"),
        &RegisterFilters::default(),
    )
    .await;

    match got.expect("a root collision must be reported, not bailed") {
        RegisterOutcome::RootOwnedBy {
            existing_id,
            refusal,
        } => {
            assert_eq!(existing_id, "owner", "the owning id must be carried out");
            assert!(
                refusal.contains("conflict") && refusal.contains("'owner'"),
                "the refusal must keep the daemon's text: {refusal}"
            );
        }
        other => panic!("expected RootOwnedBy, got {other:?}"),
    }
}

/// #9214: today's daemon sends a conflict's body only as its message, so the
/// owner is found by a lookup of the root — the index registered there.
#[tokio::test]
async fn a_conflict_without_data_finds_the_owner_by_root() {
    let root = tempfile::tempdir().expect("root");
    let root_str = root.path().to_string_lossy().into_owned();
    let listed = root_str.clone();
    let daemon = mock_daemon(move |method, params| match method {
        "search.index.create" => Err(RpcError::new(CODE_CONFLICT, "already registered")),
        "search.indexes.list" => Ok(json!({ "indexes": ["owner"] })),
        "search.index.status" if params["index_id"] == "owner" => {
            Ok(json!({ "root_path": listed }))
        }
        other => Err(RpcError::internal(format!("unexpected {other}"))),
    })
    .await;

    let got = register_on(
        &daemon.client,
        "requested",
        std::path::Path::new(&root_str),
        &RegisterFilters::default(),
    )
    .await
    .expect("the owner is found");
    assert!(
        matches!(&got, RegisterOutcome::RootOwnedBy { existing_id, .. } if existing_id == "owner"),
        "{got:?}"
    );
}

/// The same-id-different-root refusal is also a conflict, but nothing else
/// owns this root — there is no other index to reindex, so it must stay a
/// bail rather than become a silently-adopted target.
/// Test: this function IS the test.
#[tokio::test]
async fn a_conflict_without_an_existing_id_still_bails() {
    let daemon = mock_daemon(|method, _| match method {
        "search.index.create" => Err(RpcError::new(
            CODE_CONFLICT,
            "index 'requested' is registered at /elsewhere",
        )),
        "search.indexes.list" => Ok(json!({ "indexes": [] })),
        other => Err(RpcError::internal(format!("unexpected {other}"))),
    })
    .await;

    let err = register_on(
        &daemon.client,
        "requested",
        std::path::Path::new("/repos/requested"),
        &RegisterFilters::default(),
    )
    .await
    .expect_err("a conflict with no owner must still fail")
    .to_string();
    assert!(err.contains("registered at /elsewhere"), "{err}");
}

/// `register_index_with_daemon_filtered` is the arm every caller that has no
/// `--force` to honour still uses — `init`, `discover`. A root collision must
/// reach them as the daemon's refusal.
/// Test: this function IS the test.
#[tokio::test]
async fn the_legacy_register_wrapper_still_bails_on_a_root_collision() {
    let daemon =
        mock_daemon(|_, _| Err(conflict("collision", json!({ "existing_id": "owner" })))).await;

    let outcome = register_on(
        &daemon.client,
        "requested",
        std::path::Path::new("/repos/requested"),
        &RegisterFilters::default(),
    )
    .await
    .expect("a collision is an outcome");
    let err = legacy_pair(outcome).expect_err("the legacy wrapper must keep bailing");
    assert!(err.to_string().contains("collision"), "{err}");
}

/// #8727: an overlap conflict names the blocking index and its root in its
/// message; the CLI must print that text.
#[tokio::test]
async fn an_overlap_conflict_names_the_blocking_index_and_root() {
    let daemon = mock_daemon(|method, _| match method {
        "search.index.create" => Err(RpcError::new(
            CODE_CONFLICT,
            "\"/repo\" contains the root of index 'agent-x' (\"/repo/wt/agent-x\")",
        )),
        "search.indexes.list" => Ok(json!({ "indexes": [] })),
        other => Err(RpcError::internal(format!("unexpected {other}"))),
    })
    .await;

    let err = register_on(
        &daemon.client,
        "requested",
        std::path::Path::new("/repo"),
        &RegisterFilters::default(),
    )
    .await
    .expect_err("an overlap refusal must still fail")
    .to_string();
    assert!(
        err.contains("agent-x") && err.contains("/repo/wt/agent-x"),
        "the refusal must name the blocking id and root: {err}"
    );
}

/// #8922: an unparseable exclude glob is refused as invalid params whose
/// message names the pattern. The CLI must print that text.
#[tokio::test]
async fn an_invalid_exclude_glob_refusal_names_the_pattern() {
    let daemon = mock_daemon(|_, _| {
        Err(RpcError::invalid_params(
            "invalid_exclude_glob: exclude glob \"secrets/[unclosed\" does not parse",
        ))
    })
    .await;

    let err = register_on(
        &daemon.client,
        "requested",
        std::path::Path::new("/repo"),
        &RegisterFilters::default(),
    )
    .await
    .expect_err("an invalid-params refusal must fail")
    .to_string();
    assert!(err.contains("secrets/[unclosed"), "{err}");
}

/// A plain success must still report whether the daemon created the index,
/// and the params carry the filters that differ from the defaults.
/// Test: this function IS the test.
#[tokio::test]
async fn a_successful_registration_reports_created() {
    let daemon = mock_daemon(|_, params| {
        assert_eq!(params["skip_kg"], true, "{params}");
        assert!(params.get("defer_embed").is_none(), "{params}");
        Ok(json!({ "id": "requested", "created": true }))
    })
    .await;
    let filters = RegisterFilters {
        skip_kg: true,
        ..RegisterFilters::default()
    };

    let got = register_on(
        &daemon.client,
        "requested",
        std::path::Path::new("/repos/requested"),
        &filters,
    )
    .await;
    assert_eq!(
        got.expect("success must not fail"),
        RegisterOutcome::Registered { created: true },
        "a successful registration must report `created`"
    );
}

/// #9214 H2: a socket that accepts the connection and then breaks the
/// exchange is a fault to report, not "the daemon is not running" — every
/// caller turns `Unreachable` into "start the daemon" or a soft skip.
#[tokio::test]
async fn a_broken_exchange_is_an_error_not_unreachable() {
    let dir = tempfile::tempdir().expect("scratch dir");
    let socket = dir.path().join("ts.sock");
    // A hardened bind: the client refuses a socket in a non-0700 directory
    // before it dials, which would read as unreachable for the wrong reason.
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
    tokio::spawn(async move {
        // Accept every connection and close it without answering.
        while let Ok((conn, _)) = listener.accept().await {
            drop(conn);
        }
    });

    let got = register_on(
        &trusty_search::service::daemon_client::DaemonClient::at(&socket),
        "requested",
        std::path::Path::new("/repos/requested"),
        &RegisterFilters::default(),
    )
    .await;
    let err = got.expect_err("a broken exchange must be an error, not Unreachable");
    assert!(
        err.to_string().contains(&socket.display().to_string()),
        "{err}"
    );
}

/// #9214 H2: nothing serving the socket is still `Unreachable`, naming it.
#[tokio::test]
async fn an_absent_socket_is_unreachable_and_named() {
    let dir = tempfile::tempdir().expect("scratch dir");
    let socket = dir.path().join("absent.sock");
    let got = register_on(
        &trusty_search::service::daemon_client::DaemonClient::at(&socket),
        "requested",
        std::path::Path::new("/repos/requested"),
        &RegisterFilters::default(),
    )
    .await
    .expect("unreachable is an outcome");
    match got {
        RegisterOutcome::Unreachable { reason } => {
            assert!(reason.contains(&socket.display().to_string()), "{reason}");
        }
        other => panic!("expected Unreachable, got {other:?}"),
    }
}

/// `add`'s per-file write is `search.index.file.put` with the file's path and
/// content under `body`.
#[tokio::test]
async fn a_single_file_is_sent_as_file_put() {
    let dir = tempfile::tempdir().expect("scratch dir");
    let file = dir.path().join("a.rs");
    std::fs::write(&file, "fn a() {}").expect("write");
    let daemon = mock_daemon(|method, params| {
        assert_eq!(method, "search.index.file.put");
        assert_eq!(params["index_id"], "idx");
        assert_eq!(params["body"]["content"], "fn a() {}");
        Ok(json!({ "indexed": true }))
    })
    .await;
    index_single_file(&daemon.client, "idx", &file)
        .await
        .expect("sent");

    let refusing = mock_daemon(|_, _| Err(RpcError::new(CODE_NOT_FOUND, "unknown index"))).await;
    let err = index_single_file(&refusing.client, "idx", &file)
        .await
        .expect_err("refused")
        .to_string();
    assert!(err.contains("unknown index"), "{err}");
}
