//! Bounded execution of the per-palace write pipeline (issue #6366).
//!
//! Why: `PalaceHandle::remember_with_options` takes the per-palace write mutex
//! and then runs embed → vector upsert → KG persist → in-memory push → L1
//! snapshot → closet rebuild while holding it. Issue #4002 bounded the two
//! waits BEFORE the mutex is held; nothing bounded the work AFTER. A slow
//! commit therefore held the mutex for as long as it took, and every other
//! writer on that palace queued behind it — three `memory_note` calls were
//! aborted client-side after 1800 s while the daemon stayed healthy, `recall`
//! kept answering, and the writes landed server-side later. Hosting the
//! pipeline here rather than in `handle.rs` also keeps that file under the
//! 500-SLOC cap.
//!
//! What: [`remember_within`] acquires the write mutex (bounded as before),
//! then runs [`run_pipeline`] under a `tokio::time::timeout`. On expiry the
//! inner future is dropped, which releases the mutex, and the caller gets a
//! named error instead of an unbounded wait. A write that completes but takes
//! longer than [`timeouts::slow_write_warn_threshold`] is logged with the
//! palace's `kg.redb` size, so the size/duration correlation issue #6366
//! hypothesised is visible in `stderr.log` before the ceiling is ever reached.
//!
//! Cancellation contract: the timeout can only drop the pipeline at an await
//! point, and the drawer becomes durable inside `tier_c::commit_and_mirror`.
//! Three regions, with different outcomes:
//!
//! 1. Cancelled BEFORE the commit-order guard is taken — nothing was
//!    dispatched. At most an orphaned vector, which `palace_compact` reclaims.
//! 2. Cancelled WAITING for that guard — same as (1). The guard is acquired
//!    inside the budget precisely so this case aborts clean.
//! 3. Cancelled AFTER it — the commit runs to completion in a task the timeout
//!    does not own, because dropping a future cancels neither a
//!    `spawn_blocking` redb transaction nor an op already queued to the KG
//!    writer actor. The caller still gets the over-budget error, but the write
//!    lands: in redb and in the in-memory drawer table together, with the
//!    Tier C retirement invariant intact, because the guard is held across
//!    both and released only after the mirror.
//!
//! What (3) costs: the caller is told the write failed while it in fact landed,
//! and the legs after the commit — the deferred-embed spawn, the L1 snapshot,
//! the closet rebuild — are skipped. All three are caches the next write, the
//! next dream cycle, or `palace_embed_sweep` rebuild. No data is lost, and none
//! of it justifies holding the palace's write mutex indefinitely.
//!
//! Test: `write_pipeline_tests`.

use super::handle::PalaceHandle;
use super::tier_c;
use super::types::RememberOptions;
use crate::memory_core::filter::{FilterReject, check_secret, classify};
use crate::memory_core::palace::{Drawer, DrawerType, RoomType};
use crate::memory_core::room_identity::DEFAULT_WING_ID;
use crate::memory_core::store::l1_cache::L1Cache;
use crate::memory_core::store::rooms::resolve_or_create_room_in_wing;
use crate::memory_core::store::vector::VectorStore;
use crate::memory_core::timeouts;
use anyhow::{Context, Result};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Basename of the KG store inside a palace's data directory.
///
/// Why: the slow-write diagnostic reports this file's size because issue #6366
/// traced commit duration to a 326 MB `kg.redb`; naming the constant keeps the
/// diagnostic and the store's own filename from drifting apart silently.
const KG_STORE_FILENAME: &str = "kg.redb";

/// Store a new memory under an explicit ceiling on the whole critical section.
///
/// Why (issue #6366): this is the bound that stops one slow commit from
/// stalling every writer on the palace. The mutex is acquired OUTSIDE the
/// timeout — that leg has its own bound (#906/#4002) and a different failure
/// meaning — and the pipeline runs inside it, so expiry drops the pipeline
/// future, releases the mutex, and returns rather than waiting.
/// What: touches the idle clock, rejects a read-only palace, acquires the
/// per-palace write mutex, then runs [`run_pipeline`] under `budget`. Logs a
/// slow-but-successful write and returns a named error on expiry.
/// Test: `an_over_budget_write_fails_with_a_named_reason`,
/// `an_over_budget_write_releases_the_palace_write_mutex`,
/// `a_writer_queued_behind_an_over_budget_write_still_lands`.
pub(super) async fn remember_within(
    handle: &PalaceHandle,
    content: String,
    room: RoomType,
    tags: Vec<String>,
    importance: f32,
    opts: RememberOptions,
    budget: Duration,
) -> Result<Uuid> {
    // Idle-to-disk: stamp this as a genuine user access so the idle-evict
    // ticker does not drop a palace that is actively being written to.
    // No-op while a dream cycle holds `is_compacting` (see `touch`).
    handle.touch();

    // Issue #59: short-circuit before doing any embedding work when the
    // palace is opened read-only. The store layer already rejects the
    // eventual write, but returning here saves the cost of an embed
    // and surfaces a single clear error rather than an inscrutable
    // upsert failure stack.
    if handle.is_read_only() {
        return Err(anyhow::anyhow!(
            "palace '{}' is read-only: HTTP daemon holds the write lock — \
             route writes through the daemon's HTTP API or stop the daemon \
             before retrying via stdio",
            handle.id
        ));
    }

    // Issue #154: serialise mutating ops on this palace so concurrent
    // writers don't race on the L1 snapshot rename, vector upsert, KG
    // row insert, or in-memory drawer push. Held across the full
    // pipeline below. Other palaces' writes proceed in parallel.
    // Reads (`recall`, `list_drawers`, etc.) never acquire this lock,
    // so the write mutex doesn't impact read throughput.
    // Issue #906: bound the lock acquisition so a stuck embedder on one
    // writer doesn't cascade an indefinite queue of waiters.
    let _write_guard = timeouts::lock_with_timeout(
        &handle.write_mutex,
        timeouts::write_lock_timeout(),
        handle.id.as_str(),
    )
    .await?;

    // #6366: a zero or already-spent budget must not enter the pipeline at all
    // — starting work that cannot be allowed to finish would take the palace's
    // mutex for a critical section whose only outcome is an error.
    if budget.is_zero() {
        return Err(over_budget_error(handle, budget, Duration::ZERO));
    }

    let started = Instant::now();
    let outcome = tokio::time::timeout(
        budget,
        run_pipeline(handle, content, room, tags, importance, opts),
    )
    .await;
    let elapsed = started.elapsed();

    match outcome {
        Ok(result) => {
            if elapsed >= timeouts::slow_write_warn_threshold() {
                tracing::warn!(
                    palace = %handle.id,
                    elapsed_ms = elapsed.as_millis(),
                    budget_ms = budget.as_millis(),
                    kg_redb_bytes = kg_store_bytes(handle),
                    "#6366: write held the palace write mutex far longer than \
                     a write should; every other writer on this palace waited \
                     for it. A large kg.redb makes commits slower — consider \
                     compacting or splitting this palace"
                );
            }
            result
        }
        Err(_) => Err(over_budget_error(handle, budget, elapsed)),
    }
}

/// The error a write returns when it exhausts its pipeline ceiling.
///
/// Why: the reason has to name the palace, the ceiling, and the knob that moves
/// it, because the symptom an operator sees is a client-side abort with no
/// server-side evidence at all (#6366). It also reports the `kg.redb` size, the
/// factor the issue traced commit duration to.
/// What: an `anyhow::Error` carrying palace, elapsed, budget, size, and the
/// override env var.
/// Test: `an_over_budget_write_fails_with_a_named_reason`.
fn over_budget_error(handle: &PalaceHandle, budget: Duration, elapsed: Duration) -> anyhow::Error {
    anyhow::anyhow!(
        "palace '{}' write pipeline exceeded its {:?} budget after {:?} \
         (issue #6366); the palace write mutex has been released so other \
         writers proceed. kg.redb is {} bytes — a large store makes commits \
         slower; raise TRUSTY_WRITE_PIPELINE_TIMEOUT_SECS if writes on this \
         palace are legitimately this slow",
        handle.id,
        budget,
        elapsed,
        kg_store_bytes(handle).map_or_else(|| "unknown".to_string(), |bytes| bytes.to_string()),
    )
}

/// Best-effort size of the palace's KG store on disk.
///
/// Why: issue #6366 correlated a 326 MB `kg.redb` with commit durations long
/// enough to abort a client. Reporting the size beside a slow or failed write
/// is the size guard the issue asked for — it turns an invisible stall into a
/// line that names the likely cause.
/// What: `metadata(<data_dir>/kg.redb).len()`, or `None` for an in-memory
/// handle or an unreadable file. Never fails the write.
/// Test: `kg_store_bytes_is_none_for_an_in_memory_handle`.
fn kg_store_bytes(handle: &PalaceHandle) -> Option<u64> {
    let data_dir = handle.data_dir.as_ref()?;
    std::fs::metadata(data_dir.join(KG_STORE_FILENAME))
        .ok()
        .map(|m| m.len())
}

/// The embed → upsert → persist pipeline itself, unchanged from #4886.
///
/// Why: split out of [`remember_within`] so the whole body sits inside one
/// future that a timeout can drop as a unit. Nothing about the steps changed;
/// only where they run.
/// What: applies the content gates, resolves the room, builds and classifies
/// the drawer, runs Tier C admission, embeds and upserts the vector (unless
/// deferred), persists the drawer metadata, pushes it into the in-memory table,
/// spawns any deferred embed, saves the L1 snapshot, and rebuilds the closets.
/// Test: the whole `retrieval::tests` write suite exercises this.
async fn run_pipeline(
    handle: &PalaceHandle,
    content: String,
    room: RoomType,
    tags: Vec<String>,
    importance: f32,
    opts: RememberOptions,
) -> Result<Uuid> {
    // Issue #61: signal/noise gate. `force == true` bypasses the QUALITY
    // gates (noise patterns, short-content, non-alphabetic ratio) below.
    // `enforce_min_tokens` lets `memory_note` keep the noise patterns
    // while permitting short curated facts ("User prefers snake_case").
    if !opts.force {
        opts.filter
            .apply(&content, opts.enforce_min_tokens)
            .map_err(|reject| match reject {
                // Issue #1481: `PotentialSecret` carries the offending token
                // in its Display impl, so the same `{reject}` bubble names
                // the trigger for the caller to remediate.
                FilterReject::TooShort { .. }
                | FilterReject::NoisePattern { .. }
                | FilterReject::NonAlphabetic { .. }
                | FilterReject::PotentialSecret { .. } => anyhow::anyhow!("{reject}"),
            })?;
    } else if !opts.allow_secret_like {
        // Issue #2520 (two-tier force, BLOCKER fix): `force` bypasses the
        // quality gates above unconditionally, but must NEVER silently
        // bypass secret detection too — an automated writer (trusty-code's
        // per-turn recorder always sets `force: true`) would otherwise
        // persist raw credentials with zero screening. Run the secret
        // gate on its own here; only the separate, explicit
        // `allow_secret_like` opt-in — never `force` alone — skips it.
        check_secret(&content).map_err(|reject: FilterReject| anyhow::anyhow!("{reject}"))?;
    }

    // ADR-0027 T4/D4.2: the room id comes from the ROOMS registry — a
    // lookup that creates the row when absent — never from hashing a
    // `Debug` string. Fail-open: a registry error falls back to the legacy
    // fold so a room problem can never fail a memory write.
    // ADR-0027 T9: `opts.wing_id` is `None` for every caller that predates
    // wings, which resolves in the default wing — byte-identically to the
    // line this replaced.
    let wing_id = opts.wing_id.unwrap_or(DEFAULT_WING_ID);
    let room_id = resolve_or_create_room_in_wing(&handle.kg, &room, wing_id).await;

    let mut drawer = Drawer::new(room_id, content.clone());
    drawer.tags = tags;
    drawer.importance = importance.clamp(0.0, 1.0);
    // Apply classification. The caller may pre-pin the type
    // (`memory_note` always pins `UserFact`); otherwise we run the
    // heuristic classifier with `Unknown` as the fallback so
    // unclassified prose stays unlabelled rather than getting tagged
    // as `SessionEvent` by accident.
    let final_type = match opts.classify_as {
        Some(t) => t,
        None => classify(&content, DrawerType::Unknown),
    };
    drawer = drawer.with_type(final_type);

    // #4886: ADR-0028 D3/D4 Tier C admission — the single enforcement
    // point for the whole workspace. Runs after `with_type` so an admitted
    // Tier C TTL overrides the `SessionEvent` type default, and fails
    // closed, so a refusal leaves this drawer exactly as today's code would
    // have written it. See `tier_c::apply_admission`.
    tier_c::apply_admission(&mut drawer, &opts, &handle.id);
    let id = drawer.id;

    // Embed and upsert. Use the process-wide shared embedder so we don't
    // spin up a fresh ONNX session per call (issue #57). The
    // OnceCell-backed `shared_embedder` guarantees at most one model load
    // for the lifetime of the process.
    //
    // Issue #1970: a caller whose daemon is still warming up (embedder
    // cold-init in progress) sets `opts.defer_embedding` so this write
    // returns as soon as the KG/redb portion below completes instead of
    // blocking behind a 30-120s ONNX/CoreML compile — the vector is
    // backfilled by a background task once the embedder resolves. Text
    // and KG indexing never depend on the embedder either way; this
    // branch only changes *when* the drawer becomes vector-searchable.
    //
    // #4906: the deferred branch is SPAWNED BELOW, after the drawer reaches
    // `handle.drawers`. Spawning here raced the push: the background task
    // refuses to record a failure for a drawer absent from that table, so a
    // task that failed before the push wrote nothing for a drawer that was
    // about to become durable and vector-less — the exact combination this
    // lane exists to eliminate.
    if !opts.defer_embedding {
        // Issue #906: both `shared_embedder()` (cold init path) and
        // `embed_batch` carry their own bounded timeouts — if the embedder
        // hangs mid-batch the remember call returns an error instead of
        // blocking the write-lock indefinitely. #6366: those two legs are
        // additionally capped as a whole by the caller's pipeline budget.
        let embedder = super::embedder::shared_embedder()
            .await
            .context("acquire shared embedder for remember")?;
        let embed_timeout = timeouts::embed_batch_timeout();
        let vecs = tokio::time::timeout(
            embed_timeout,
            // `from_ref` rather than `&[content]`: the deferred branch below
            // still needs `content`, so this borrows instead of moving.
            embedder.embed_batch(std::slice::from_ref(&content)),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "embed_batch timed out after {:?} on remember path (issue #906); \
                     increase TRUSTY_EMBED_BATCH_TIMEOUT_SECS if batches legitimately \
                     take longer on this host",
                embed_timeout
            )
        })?
        .context("embed drawer content")?;
        if let Some(v) = vecs.into_iter().next() {
            handle
                .vector_store
                .upsert(id, v)
                .await
                .context("upsert drawer vector")?;
        }
    }

    // Persist drawer metadata BEFORE the in-memory push so a crash mid-op
    // cannot leave an in-memory drawer with no redb record backing it.
    // #4886: when this drawer claims a `fact_key`, the same call retires
    // the slot's prior occupant in the SAME redb transaction.
    //
    // #6366: the commit and its in-memory mirror are ONE indivisible step, for
    // the reason `tier_c::commit_and_mirror` documents — a dropped future does
    // not stop a `spawn_blocking` redb transaction or an op already queued to
    // the KG writer actor, so splitting them let the next writer read a moved
    // index against an unmirrored drawer table and leave two claimants on one
    // `fact_key`. Two things keep that unreachable:
    //
    //   1. The commit-order guard is taken HERE, while still inside the
    //      caller's budget. A write that exhausts its budget waiting for it has
    //      dispatched nothing, so it aborts cleanly with no durable trace.
    //   2. Once taken, the guard and the commit move into a `tokio::spawn`ed
    //      task. The caller's timeout can drop the JoinHandle, but not the
    //      task — so an abandoned commit still lands in redb AND in
    //      `handle.drawers`, and still holds the guard until both are done.
    //
    // The guard is deliberately NOT the write mutex: that one must release on
    // timeout (it is what unblocks the queue), and this one must not.
    let order = handle.commit_mutex.clone().lock_owned().await;
    tokio::spawn(tier_c::commit_and_mirror(
        handle.commit_ctx(),
        drawer,
        order,
    ))
    .await
    .context("write pipeline commit task join")??;

    // #4906: spawn the background embed only now. The drawer is in redb and
    // in the in-memory table, so a failure arriving at any point from here
    // on finds the drawer present and gets recorded. See the branch above.
    if opts.defer_embedding {
        handle.spawn_deferred_embed(id, content);
    }

    // L1 snapshot: re-sort the in-memory table and persist top-15.
    if let Some(data_dir) = handle.data_dir.as_ref() {
        let snap = handle.drawers.read().clone();
        L1Cache::save_l1_cache(&snap, data_dir).context("save L1 snapshot")?;
    }

    // Refresh the closet keyword index so L2 tag-boosting picks up the
    // new drawer without waiting for a dream cycle.
    handle.rebuild_closets();

    Ok(id)
}
