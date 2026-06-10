# CUDA Embedder Regression — trusty-search/trusty-embedderd 0.23.6

**Date:** 2026-06-05  
**Versions affected:** trusty-search 0.23.5–0.23.6, trusty-embedderd 0.3.2, trusty-common 0.14.0  
**Last-known-good version:** trusty-search 0.23.4 / trusty-embedderd 0.3.1 / trusty-common 0.13.0 (commit `e758c82`, before `258cd05`)  
**Failure modes observed:** STALLED, HUNG, ZERO-VECTOR (all-zeros embeddings)  
**Investigation scope:** READ-ONLY, no builds, no host access  

---

## Executive Summary

Three failure modes all trace to the same two root causes introduced in commit
`258cd05` (PR #755, merged 2026-06-04):

1. **Primary — Reader-task death leaves callers hung permanently:** The
   `reader_task` in the new multi-flight `StdioEmbedderClient` exits on the
   first response timeout but does NOT restart. Subsequent `embed_batch` calls
   push to the pending queue and write to stdin, but no task reads stdout. The
   `reply_rx.await` in each caller hangs forever because the oneshot sender is
   held in the pending queue with no consumer. On CUDA this timeout fires much
   faster than on ANE because CUDA ORT sessions are slower to initialise and
   the 16 GB T4 BFCArena over-reservation (partially fixed by #600) causes the
   very first large batch to stall.

2. **Secondary — batch-64 default without a CUDA-aware sidecar cap:** The
   `DEFAULT_BATCH_SIZE` in `trusty-embedderd/src/batch_queue.rs` was doubled
   from 32 to 64 (commit `258cd05`) on ANE/M4 Max empirical data. The sidecar
   now also receives `TRUSTY_EMBED_BATCH_SIZE=64` forwarded by `do_spawn`
   (commit `e758c82`, #748), then `tune_batch_size_for_provider` bumps
   `TRUSTY_MAX_BATCH_SIZE` to 512 on a CUDA build — so the parent sends 512-
   chunk waves but the sidecar's `BatchQueue` processes them 64 at a time,
   producing multiple ONNX calls per wave. Combined with 2 inflight waves
   (default `TRUSTY_EMBED_INFLIGHT=2`), the BFCArena on a 16 GB T4 sees two
   concurrent 64-chunk sessions drawing from its 12 GiB cap simultaneously —
   the same arena-OOM scenario issue #600 addressed, now re-triggered by
   concurrent inflight sessions.

3. **Zero-vector failure — silent CUDA EP fallback:** When the CUDA EP fails
   to initialise (insufficient VRAM, driver error, or OOM during the warmup
   batch in `with_cache_size`), `FastEmbedder::with_cache_size` falls back to
   CPU silently (unless `TRUSTY_DEVICE=gpu` is set). The CPU `TextEmbedding`
   session starts fine; warmup succeeds; the model is loaded. However, with
   `TRUSTY_DEVICE` not set to `gpu` the operator never sees an error — they
   only notice zero performance. This is NOT the source of zero-vectors. The
   actual zero-vector failure is caused by the sidecar's `batch_worker` in
   `trusty-embedderd/src/batch_queue.rs` when the ONNX session OOMs mid-batch:
   it replies `Err(anyhow!("embedder failed: …"))` to each pending request, and
   the parent-side `embed_chunks_in_batches` propagates this as `Err`. The
   zero-vectors therefore come from a higher-level caller that catches and
   silences this error — see Root Cause 3 below.

---

## Ranked Root-Cause Analysis

### Root Cause 1 (MOST LIKELY — explains HUNG): reader_task exits on timeout and is never restarted

**Introduced by:** commit `258cd05` (PR #755)  
**File:** `crates/trusty-common/src/embedder_client/stdio.rs`

The `reader_task` is spawned once in `StdioEmbedderClient::new` and runs as a
detached Tokio task. On a timeout (default 120 s, env `TRUSTY_EMBEDDERD_CALL_TIMEOUT_SECS`):

```rust
// stdio.rs:201-212
Err(_elapsed) => {
    tracing::warn!(
        timeout_secs = timeout.as_secs(),
        "StdioEmbedderClient reader: timed out waiting for response \
         (sidecar may be stalled) — draining pending requests"
    );
    drain_pending_with_error(
        &pending,
        EmbedderError::Stdio(format!(
            "embed call timed out after {}s — sidecar may be stalled …",
            timeout.as_secs()
        )),
    )
    .await;
    return;  // ← TASK EXITS HERE
}
```

After `return`, the reader task is gone. The `pending: PendingQueue` is still
alive (shared via `Arc`), and the `stdin: Arc<Mutex<ChildStdin>>` is still
live. Subsequent `embed_batch` calls (from the still-running reindex pipeline)
execute the following sequence:

```rust
// stdio.rs:334-388
let _permit = self.inflight.acquire().await;  // succeeds (semaphore not closed)
let (reply_tx, reply_rx) = oneshot::channel();
{
    let mut guard = self.pending.lock().await;
    guard.push_back(PendingRequest { sent, reply: reply_tx });
}
// write request to stdin — succeeds (sidecar is still alive, reading stdin)
stdin_guard.write_all(&payload).await;
stdin_guard.flush().await;

// ← THE HANG: reply_rx.await blocks forever
// The reader task is dead. reply_tx sits in the pending queue with no consumer.
// The sidecar processes the request and writes the response to stdout.
// Nobody reads stdout. reply_tx is never dropped. reply_rx never resolves.
let result = reply_rx.await.map_err(|_| { … })?;
```

The hang is PERMANENT: the sidecar keeps running, writing responses to stdout,
the reindex pipeline keeps writing requests to stdin, but reply_rx never fires.
The supervisor's `supervision_loop` only watches the child process exit via
`child.wait()` — it does NOT detect a dead reader task. So no restart is
triggered. The daemon logs silence. The SSE stream stalls.

**Why this hits CUDA harder than ANE:** On Apple Silicon ANE the 120-second
timeout is generous enough that a batch completes before the deadline under most
load. On CUDA the ORT BFCArena over-reservation (issue #600) can cause the
first large batch (now 64 chunks by default, or 512 after
`tune_batch_size_for_provider`) to stall waiting for VRAM allocation, easily
exceeding 120 s on a loaded T4. The very first timeout kills the reader task
permanently, and all subsequent batches hang.

---

### Root Cause 2 (LIKELY — explains STALL): batch-64 + INFLIGHT=2 re-triggers BFCArena OOM on CUDA

**Introduced by:** commit `258cd05` (PR #755) + commit `e758c82` (PR #748)  
**Files:** `crates/trusty-embedderd/src/batch_queue.rs`, `crates/trusty-search/src/commands/start.rs`, `crates/trusty-common/src/embedder_client/supervisor.rs`

**The batch-64 change:**

```rust
// batch_queue.rs:46
// Before #755:
pub const DEFAULT_BATCH_SIZE: usize = 32;
// After #755:
pub const DEFAULT_BATCH_SIZE: usize = 64;
```

Rationale in the commit is M4 Max empirical data. The CUDA path was not tested.

**The forwarding chain on CUDA:**

1. `tune_batch_size_for_provider(Cuda)` in `start.rs` sets
   `TRUSTY_MAX_BATCH_SIZE=512` (GPU_BATCH_DEFAULT):

```rust
// start.rs (fn tune_batch_size_for_provider)
unsafe {
    std::env::set_var("TRUSTY_MAX_BATCH_SIZE", GPU_BATCH_DEFAULT.to_string());
    // GPU_BATCH_DEFAULT = 512
}
```

2. `do_spawn` calls `embed_batch_size()` which reads `TRUSTY_MAX_BATCH_SIZE=512`,
   then calls `sidecar_batch_size(512, is_coreml=false, coreml_cap)` which
   returns 512 unchanged (non-CoreML path passes through):

```rust
// supervisor.rs (sidecar_batch_size)
pub fn sidecar_batch_size(resolved: usize, is_coreml: bool, coreml_cap: usize) -> usize {
    let raw = if is_coreml {
        resolved.min(coreml_cap)
    } else {
        resolved  // ← CUDA path: 512 passed through
    };
    raw.max(1)
}
```

3. `spawn_child` forwards `TRUSTY_EMBED_BATCH_SIZE=512` to the sidecar.
   The sidecar's `BatchConfig::batch_size` is set to 512.

4. `embed_chunks_in_batches` builds waves of `inflight=2` sub-batches of 512
   chunks each, dispatching them concurrently via `futures::stream::buffered(2)`:

```rust
// ingest.rs:617-645
while wave_sub_batches.len() < inflight && wave_pos < chunk_total {
    let end = (wave_pos + batch_size).min(chunk_total);
    // batch_size = 512 on CUDA
    let sub: Vec<String> = chunks[wave_pos..end].iter().map(|c| c.content.clone()).collect();
    wave_sub_batches.push((wave_pos, sub));
    wave_pos = end;
}
// dispatches 2 concurrent embed_batch calls of 512 chunks each
futures::stream::iter(iter).buffered(inflight).collect().await
```

5. Each `embed_batch` call goes through `StdioEmbedderClient`, which writes to
   the sidecar's stdin. The sidecar's `batch_worker` accepts them into its
   512-slot `BatchQueue`. The ONNX session runs a 512-chunk inference.

**The OOM interaction:** With `TRUSTY_GPU_MEM_LIMIT_BYTES` defaulting to 12 GiB
and `arena_extend_strategy=kSameAsRequested` (from `build_cuda_provider` in
`embedder/mod.rs`), a single 512-chunk batch on AllMiniLML6V2Q (384-dim INT8)
draws roughly 512 × (384 × 4 bytes) = 768 KB of output tensor, plus ORT's
workspace. That alone is fine. The issue is that with `INFLIGHT=2` the sidecar
receives two 512-chunk requests nearly simultaneously. The `BatchQueue` is
designed to coalesce but in practice the 10ms window collapses two separate
512-chunk calls into one 512-chunk ONNX inference (the queue caps at 512). The
ONNX session is serial. However the two in-flight `embed_batch` callers are both
blocking on `reply_rx.await` while both permits are held. This means the
semaphore is fully consumed (INFLIGHT=2 permits both held). The `reader_task`
must process 2 sequential ONNX responses. If either ONNX call stalls (arena
allocation delay, kernel launch queue), the 120s timeout fires before the second
response arrives, killing the reader task.

The more direct issue: `batch_queue.rs` has `PENDING_CHANNEL_CAPACITY=512`, so
with INFLIGHT=2 sending 512-chunk batches to the sidecar's 512-slot queue,
every additional request to the sidecar (from the second in-flight wave) piles
up behind the first. The sidecar processes them serially. The parent's
`buffered(2)` awaits both results before moving to the next wave. If the first
ONNX call takes >60s (BFCArena over-reservation on T4) the timeout fires.

---

### Root Cause 3 (LIKELY — explains ZERO-VECTORS): silent error swallowing upstream of embed

**NOT introduced by #755** — this is a pre-existing silent-failure bug that the
new failure modes expose more frequently.

The `batch_worker` in `batch_queue.rs` correctly returns `Err` to callers:

```rust
// batch_queue.rs (batch_worker, error arm)
Err(e) => {
    let msg = format!("embedder failed: {e:#}");
    tracing::error!("{msg}");
    for pending in batch {
        let _ = pending.reply.send(Err(anyhow!(msg.clone())));
    }
}
```

And `embed_chunks_in_batches` propagates the error:

```rust
// ingest.rs:648
let batch_vecs = vecs.context("batch embed_batch failed")?;
```

However the zero-vector failure is more subtle. It arises from the CUDA EP
silent fallback in `FastEmbedder::with_cache_size`:

```rust
// embedder/mod.rs:596-646
let (m, provider) = match TextEmbedding::try_new(q_opts) {
    Ok(m) => (m, q_provider),
    Err(q_err) => {
        if q_provider != ExecutionProvider::Cpu && !require_gpu {
            tracing::warn!(
                "{} EP init failed ({q_err:#}); retrying with CPU-only \
                 execution provider",
                q_provider
            );
            unsafe { std::env::set_var("TRUSTY_DEVICE", "cpu") };
            let (cpu_opts, cpu_provider) =
                Self::init_options(EmbeddingModel::AllMiniLML6V2Q);
            match TextEmbedding::try_new(cpu_opts) {
                Ok(m) => (m, cpu_provider),
                Err(cpu_err) => { … fallback to AllMiniLML6V2 … }
            }
        }
    }
}
```

When CUDA EP init fails (VRAM OOM during model load, no device), the sidecar
falls back to CPU. The CPU session produces real embeddings, not zeros. The
zero-vector failure must therefore come from one of:

**a) ORT CUDA EP session fallback returning zero tensors.** When ORT registers
the CUDA EP but the device is unavailable at inference time (not init time), ORT
silently falls back to CPU for individual ops. In rare configurations (CUDA
driver present but GPU busy/reset) this can return an all-CPU inference where
ORT's output buffer for the CUDA EP path is zero-initialised and not written.
This is an ORT-internal silent failure — not visible in trusty code.

**b) The `StdioEmbedderClient` returning an empty or zero embedding on a
race in the new reader_task.** There is one specific race: if the reader task
receives a response frame and the `pending` queue is empty at that instant
(spurious frame detection):

```rust
// stdio.rs:263-270
let req = {
    let mut guard = pending.lock().await;
    guard.pop_front()
};
let Some(pending_req) = req else {
    tracing::warn!(
        "StdioEmbedderClient reader: received response but pending queue is empty \
         (spurious frame from sidecar?) — ignoring"
    );
    continue;  // ← RESPONSE DROPPED. The caller's reply_rx NEVER fires.
};
```

If the pending-queue pop races with the write path (request written to stdin
before `push_back` completes because of the lock ordering), a response frame is
dropped. The caller's `reply_rx.await` blocks, then the next response for the
NEXT request is popped by the wrong `pending_req` — wrong `sent` count. This
yields `DimensionMismatch` which propagates as an error, not zero-vectors.

**c) The actual zero-vector source:** When the reader task has already exited
(Root Cause 1) and callers are hanging on `reply_rx.await`, the reindex
pipeline's outer timeout or cancellation token fires. The `embed_chunks_in_batches`
call is cancelled; the `Option<Vec<f32>>` slots for those chunks remain `None`.
Upstream code that does not propagate the error but instead substitutes a default
(zero vector) produces the all-zeros embedding. This is NOT in the current
trusty-search code — `embed_chunks_in_batches` returns `Err`. But if the caller
of `embed_chunks_in_batches` is inside a `tokio::spawn` that gets dropped and
the caller catches the JoinError as Ok(None-filled result), zeros appear.

**Summary on zero-vectors:** The most likely explanation for all-zeros is Root
Cause 1 causing a permanent hang, which triggers a higher-level timeout that
exits the indexing task without propagating the error, leaving zero-filled
embedding slots in the final index. This is a SILENT FAILURE bug.

---

## Zero-Vector: Is This a Silent-Failure Bug?

**Yes.** There are two independent silent-failure risks:

1. `init_options` / `with_cache_size` silently falls back from CUDA to CPU
   when the CUDA EP fails. The sidecar logs `tracing::warn!` but the parent
   sees provider=CUDA on `/health` (predicted, not actual). No error is
   returned to the operator unless `TRUSTY_DEVICE=gpu`.

2. When `reader_task` exits and callers hang, there is no timeout at the
   `embed_chunks_in_batches` level — the hang is infinite unless the Tokio
   runtime is shut down. The reindex SSE stream goes silent until the OS drops
   the TCP connection. The operator sees "indexing stalled" with no error log
   (because the reindex task is blocked in `reply_rx.await`, not panicking).

Both must be fixed to satisfy the project's design-for-failure principle (loud
errors, no silent degradation).

---

## Last-Known-Good Version / Rollback Candidate

| Version | trusty-search | trusty-embedderd | trusty-common | Commit |
|---------|---------------|------------------|---------------|--------|
| **LKG** | 0.23.4 | 0.3.1 | 0.13.0 | `e758c82` |
| Broken | 0.23.5 | 0.3.2 | 0.14.0 | `258cd05` |
| Current | 0.23.6 | 0.3.2 | 0.14.0 | `1923ead` (origin/main HEAD) |

The embedding path was unchanged between 0.23.4 and the subsequent 0.23.5+
commits (0.23.5 = `258cd05` multi-flight, 0.23.6 = `89e84b4` progress). Rollback
target is **trusty-search 0.23.4 / trusty-embedderd 0.3.1 / trusty-common
0.13.0** at commit `e758c82`.

The #748 changes (batch forwarding, `sidecar_batch_size`) are in 0.23.4 and are
SAFE for CUDA — they only affect the batch size forwarded to the sidecar, not
the multi-flight client design.

---

## Immediate Safe-Config Workaround for 0.23.6 on CUDA

Apply these environment variables in the trusty-search daemon environment (e.g.
`~/.trusty-search/daemon.env` or the launchd/systemd unit):

### Step 1 — Force serial single-flight embedding (disables multi-flight)

```bash
TRUSTY_EMBED_INFLIGHT=1
```

**Effect:** `embed_inflight()` in `stdio.rs` clamps to `[1, 4]`, default 2. Setting
to 1 restores the serial (one in-flight) behavior. The `StdioEmbedderClient`
sends one batch, awaits its response, then sends the next. The reader task still
exists and can still exit on timeout, but there is now only one pending request
at a time, reducing the deadlock window dramatically.

**IMPORTANT NOTE:** Setting `TRUSTY_EMBED_INFLIGHT=1` does NOT fix Root Cause 1
(reader task exit). It only reduces the frequency. See code fix below.

### Step 2 — Reduce batch size to avoid BFCArena OOM

```bash
TRUSTY_MAX_BATCH_SIZE=16
TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1
TRUSTY_EMBED_BATCH_SIZE=16
```

**Effect:**
- `TRUSTY_MAX_BATCH_SIZE=16` sets the parent pipeline batch size.
- `TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1` bypasses the tier hard cap so the value
  is not clamped up to 128/256/512.
- `TRUSTY_EMBED_BATCH_SIZE=16` overrides what `do_spawn` forwards to the
  sidecar (the forwarded value is computed from `embed_batch_size()` which
  reads `TRUSTY_MAX_BATCH_SIZE`; setting both ensures the sidecar also uses 16).

NOTE: `TRUSTY_EMBED_BATCH_SIZE` is read by the sidecar's `Args` parser (the
`--batch-size` default reads `env = "TRUSTY_EMBED_BATCH_SIZE"`). Setting it
in the parent's env AND forwarding via supervisor ensures consistency.

### Step 3 — Set conservative CUDA VRAM limit

```bash
TRUSTY_GPU_MEM_LIMIT_BYTES=6442450944
```

(= 6 GiB; leaves 10 GiB for CUDA context + cuDNN workspaces on a 16 GB T4)

**Effect:** `resolve_cuda_options()` reads this and `build_cuda_provider()` sets
`gpu_mem_limit` on the ORT CUDA EP, preventing BFCArena from grabbing all VRAM.

### Step 4 — Extend per-call timeout

```bash
TRUSTY_EMBEDDERD_CALL_TIMEOUT_SECS=600
```

**Effect:** gives the reader task 10 minutes before it times out and exits.
This does NOT fix Root Cause 1 (the task still exits permanently on timeout)
but dramatically reduces the frequency on a loaded T4. Do NOT set this below
the expected worst-case ONNX call time for your batch size.

### Step 5 — Force CUDA EP (prevent silent CPU fallback)

```bash
TRUSTY_DEVICE=gpu
```

**Effect:** `FastEmbedder::with_cache_size` sets `require_gpu=true`, which causes
the sidecar to return a hard error instead of silently falling back to CPU. This
surfaces the true failure mode instead of producing slow CPU embeddings that look
like CUDA embeddings on `/health`.

### Complete safe-config block

```bash
# trusty-search CUDA safe config for 0.23.6 on a 16 GB T4
TRUSTY_EMBED_INFLIGHT=1
TRUSTY_MAX_BATCH_SIZE=16
TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1
TRUSTY_EMBED_BATCH_SIZE=16
TRUSTY_GPU_MEM_LIMIT_BYTES=6442450944
TRUSTY_EMBEDDERD_CALL_TIMEOUT_SECS=600
TRUSTY_DEVICE=gpu
```

**Can multi-flight be disabled without a code change?** YES — `TRUSTY_EMBED_INFLIGHT=1`
reduces it to single-flight at the StdioEmbedderClient level. The wave loop in
`embed_chunks_in_batches` also reads `TRUSTY_EMBED_INFLIGHT` and builds waves of
1 sub-batch at a time, so setting this to 1 effectively restores serial behavior
across both layers.

**WARNING:** This workaround reduces throughput significantly. On CUDA with batch=16,
embedding 44k chunks will take much longer. The correct fix is the code change below.

---

## Proposed Code Fix

### Fix 1 (CRITICAL): Restart reader_task on timeout instead of returning

**File:** `crates/trusty-common/src/embedder_client/stdio.rs`

The reader task must NOT exit on timeout. Instead, it should drain pending
requests (correct) and then continue looping, waiting for the next response.
However, since the sidecar may have written the stalled response to stdout (the
bytes are in the pipe buffer), the reader must also consume and discard the stale
response before proceeding.

The cleaner fix is for the reader task timeout to signal the supervisor to respawn
the sidecar. The supervisor already handles this for process crashes (EOF on
stdout). The reader task should simulate an EOF by returning from the loop on
timeout, but the supervisor must detect this and respawn.

**Current behavior:** `reader_task` returns on timeout → supervisor never wakes
(it only watches `child.wait()`, not the reader task future).

**Fix:** Close the `BufReader<ChildStdout>` on timeout by dropping the reader.
This causes the sidecar's stdout pipe to have its read end closed. The sidecar
will get `SIGPIPE` or `write: broken pipe` on its next response write and exit.
`child.wait()` then fires in `supervision_loop`, which restarts the sidecar with
a fresh `StdioEmbedderClient` (and a new reader task).

The current drain-and-return IS correct behavior — the fix is just ensuring the
supervision loop detects the reader task's exit.

**Minimal fix (least change):** Add a `Notify` or `CancellationToken` that
`reader_task` fires when it exits for any reason (timeout, EOF, IO error). Have
`supervision_loop` `tokio::select!` on both `child.wait()` and this notification:

```rust
// In StdioEmbedderClient::new:
let reader_exited = Arc::new(tokio::sync::Notify::new());
let reader_exited_clone = Arc::clone(&reader_exited);
tokio::spawn(async move {
    reader_task(BufReader::new(stdout), pending_clone).await;
    reader_exited_clone.notify_one();  // wake supervisor
});
// Pass reader_exited to the supervisor so it can kill + respawn the sidecar.
```

A simpler variant that requires no API change: convert the timeout path in
`reader_task` from `return` to a `continue` that discards any partial line
buffered and then re-arms the timeout. This is only safe if the sidecar has NOT
written a partial response (the stale bytes must be consumed). Given ORT ONNX
calls are atomic (either the full response is written or none is), it is safe to
drain the current `line` buffer and continue:

```rust
// stdio.rs, reader_task, timeout arm — PROPOSED FIX:
Err(_elapsed) => {
    tracing::warn!(
        timeout_secs = timeout.as_secs(),
        "StdioEmbedderClient reader: timed out waiting for response \
         — draining pending requests and dropping current line buffer"
    );
    drain_pending_with_error(&pending, EmbedderError::Stdio(format!(
        "embed call timed out after {}s",
        timeout.as_secs()
    ))).await;
    // Do NOT return — drop the partial line buffer and re-arm.
    // The sidecar will eventually complete the in-progress ONNX call
    // and write the response; we discard it (pop will find empty queue
    // and log "spurious frame" — harmless).
    line.clear();
    // continue; ← implicit, loop re-enters
}
```

This is the **minimal safe fix**: the reader task stays alive, subsequent
embed_batch calls work normally, and the orphaned sidecar response is consumed
and discarded as a spurious frame.

### Fix 2 (IMPORTANT): Add a CUDA-aware batch size cap in the sidecar forwarding

**File:** `crates/trusty-common/src/embedder_client/supervisor.rs`

The `sidecar_batch_size` function passes `resolved` through unchanged for
non-CoreML providers. On CUDA, `resolved` is 512 (from `tune_batch_size_for_provider`).
The fix is to cap the sidecar batch size for CUDA at a safe value that keeps
the ORT BFCArena below `gpu_mem_limit`:

```rust
pub fn sidecar_batch_size(
    resolved: usize,
    is_coreml: bool,
    coreml_cap: usize,
    is_cuda: bool,
    cuda_cap: usize,
) -> usize {
    let raw = if is_coreml {
        if coreml_cap == 0 { tracing::warn!(…); }
        resolved.min(coreml_cap)
    } else if is_cuda {
        resolved.min(cuda_cap)  // NEW: cap CUDA sidecar batch size
    } else {
        resolved
    };
    raw.max(1)
}
```

`cuda_cap` defaults to a conservative value (e.g. 64) that keeps two concurrent
ONNX inference calls within the 12 GiB VRAM budget on a 16 GB T4. This is
separate from `TRUSTY_MAX_BATCH_SIZE` (which controls the parent pipeline wave
size) and `TRUSTY_GPU_MEM_LIMIT_BYTES` (which caps the ORT arena).

### Fix 3 (IMPORTANT): Loud failure on zero-vector / silent-fallback

**File:** `crates/trusty-common/src/embedder/mod.rs`

The CPU fallback in `with_cache_size` should emit a structured event that
propagates to the reindex pipeline status, not just a `tracing::warn!`. When
the sidecar falls back from CUDA to CPU, the provider reported by the parent
(`resolve_expected_provider()` returns `Cuda`) does not match the actual
provider used by the sidecar (`Cpu`). The `/health` endpoint reports `CUDA`
while inference runs on CPU — a silent mismatch.

**Fix:** Add a health-check flag file or a startup probe response field that
the parent reads to confirm the actual provider. Alternatively, set an env var
in the sidecar on fallback (e.g. `TRUSTY_ACTUAL_PROVIDER=cpu`) and have the
parent log a prominent warning during the startup probe. This is a larger change
and can be tracked separately.

---

## Supporting Evidence

### Commit history of the embedding path (0.23.x line)

| Commit | PR | Version | Change | CUDA risk |
|--------|----|---------|--------|-----------|
| `22206d5` | #746 | 0.23.3 | overlap embedder warm-up with chunking | LOW |
| `e758c82` | #748 | 0.23.4 | forward embed batch size to sidecar (CoreML-capped) | LOW (CUDA uncapped) |
| `258cd05` | #755 | 0.23.5 | multi-flight pipelined embed + batch-64 default | **CRITICAL** |
| `89e84b4` | #758 | 0.23.6 | finer indexing progress (32-chunk advance) | LOW |

### Key env-var resolution chain on CUDA (0.23.6)

```
startup: MemoryPolicy::detect() → TRUSTY_MAX_BATCH_SIZE=96 (Medium tier, 16 GB)
tune_batch_size_for_provider(Cuda) → TRUSTY_MAX_BATCH_SIZE=512
do_spawn: embed_batch_size()=512, is_coreml=false
  → sidecar_batch_size(512, false, coreml_cap) = 512
  → TRUSTY_EMBED_BATCH_SIZE=512 forwarded to sidecar
sidecar: BatchConfig::batch_size=512, DEFAULT_BATCH_SIZE=64 (ignored, env wins)
parent: embed_chunks_in_batches with inflight=2, batch_size=512
  → wave = [sub-batch A: 512 chunks, sub-batch B: 512 chunks]
  → two concurrent embed_batch calls via StdioEmbedderClient
sidecar: receives two 512-chunk requests back-to-back (10ms window coalesces)
  → 512-chunk ONNX inference × 2 sequential calls
  → BFCArena peak ≈ 2 × (512 × per-slot ORT workspace)
  → may exceed 6-12 GiB on T4 → STALL → 120s timeout → reader_task exits → HANG
```

### `_permit` drop timing — NOT a bug

The `_permit` in `embed_batch` is held through `reply_rx.await` (line 381) and
dropped when the function returns. This is by design: the semaphore bounds total
in-flight batches at the PARENT side. It does not cause deadlock in isolation.
The deadlock only occurs when `reader_task` exits while callers hold permits and
are waiting on `reply_rx.await`.

---

## Answers to Specific Questions

### Q1: Where could the embedder return all-zero vectors silently?

There is no code path in `embed_chunks_in_batches` that produces zero vectors
without an error. The zero-vector outcome requires a caller ABOVE
`embed_chunks_in_batches` to catch and suppress its `Err` return, substituting
`None`-filled (or zero-filled) embedding slots. The ONNX session itself does not
produce zeros on CUDA EP fallback — it produces real CPU embeddings.

The most likely zero-vector scenario is: permanent hang → higher-level timeout
cancels the indexing task → partial `Vec<Option<Vec<f32>>>` with `None` slots
is stored or default-substituted somewhere upstream. This is a consequential
silent failure, not an intrinsic zero-vector bug in the embedding path.

The `resolve_expected_provider()` / actual provider mismatch (CUDA predicted,
CPU actual) produces correct non-zero vectors but from the CPU, not the GPU. This
is not zero-vectors but is a silent performance regression.

### Q2: Does the multi-flight pipeline deadlock on CUDA?

YES, but specifically via the reader-task-exit path, not a channel deadlock.
The `buffered(inflight)` approach is correct for FIFO ordering. The deadlock
arises because `reader_task` exits permanently on timeout while callers continue
to push to the pending queue and write to stdin. The supervision loop does not
detect reader task exit.

### Q3: Batch sizing on CUDA in 0.23.6

- **Default without env vars:** `tune_batch_size_for_provider` sets
  `TRUSTY_MAX_BATCH_SIZE=512`. Forwarded to sidecar as `TRUSTY_EMBED_BATCH_SIZE=512`.
- **Is it capped by `TRUSTY_GPU_MEM_LIMIT_*`?** NO. `TRUSTY_GPU_MEM_LIMIT_*`
  caps the ORT BFCArena ceiling, not the batch size. The batch size is not
  reduced when VRAM is constrained — that is the bug.
- **Is the env-var cap wired into the embedderd sidecar?** YES for
  `TRUSTY_GPU_MEM_LIMIT_BYTES` and `TRUSTY_GPU_MEM_LIMIT_MB` — both are read
  by `resolve_cuda_options()` in `embedder/mod.rs`, which is called by
  `build_cuda_provider()` in the sidecar's `FastEmbedder::init_options`. So the
  VRAM cap applies to the sidecar's ORT session.
- **Does batch-64 + BFCArena over-reserve on T4?** Potentially YES with
  INFLIGHT=2. Two concurrent 64-chunk ONNX calls in a 12 GiB arena with
  `kSameAsRequested` growth should be fine; the issue is the 512-chunk wave size
  forwarded from the parent pipeline (not the sidecar's internal 64-chunk
  BatchQueue grouping). The sidecar receives 512 texts, the BatchQueue caps at
  512, runs one 512-chunk ONNX call — that single call is what risks VRAM OOM.

### Q4: Exact env vars for CUDA safe config

Listed in the workaround section above. Summary:

| Env var | Value | Purpose |
|---------|-------|---------|
| `TRUSTY_EMBED_INFLIGHT` | `1` | Force serial single-flight (disables multi-flight pipelining) |
| `TRUSTY_MAX_BATCH_SIZE` | `16` | Reduce parent pipeline wave size |
| `TRUSTY_MAX_BATCH_SIZE_EXPLICIT` | `1` | Bypass tier hard cap (prevents clamping back up) |
| `TRUSTY_EMBED_BATCH_SIZE` | `16` | Explicitly set sidecar batch size (belt-and-suspenders) |
| `TRUSTY_GPU_MEM_LIMIT_BYTES` | `6442450944` | 6 GiB VRAM cap for BFCArena |
| `TRUSTY_EMBEDDERD_CALL_TIMEOUT_SECS` | `600` | Extend timeout to 10 min |
| `TRUSTY_DEVICE` | `gpu` | Prevent silent CPU fallback |

No env var exists to fully prevent the reader_task from dying on timeout — that
requires the code fix (Fix 1). `TRUSTY_EMBEDDERD_CALL_TIMEOUT_SECS=600` reduces
the risk but does not eliminate it.

---

## Conclusion

The 0.23.6 CUDA regression is a critical design flaw in the multi-flight
`StdioEmbedderClient` introduced in commit `258cd05` (PR #755): the background
reader task exits permanently on a single timeout, and no mechanism exists to
restart it or alert the supervision loop. Combined with the batch-64 default and
the 512-chunk CUDA wave size, a 16 GB T4 will reliably trigger the 120-second
timeout on the first large wave, permanently killing the reader task and hanging
all subsequent embedding work.

The rollback target is **trusty-search 0.23.4 / trusty-embedderd 0.3.1 /
trusty-common 0.13.0** at commit `e758c82`. The safe-config workaround
(`TRUSTY_EMBED_INFLIGHT=1`, small batch size, conservative VRAM limit) reduces
the probability of hitting the timeout on 0.23.6 but does not eliminate it.
The code fix (Fix 1: reader task loop instead of exit on timeout) is the correct
permanent solution.
