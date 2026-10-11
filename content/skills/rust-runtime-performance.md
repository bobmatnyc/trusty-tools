---
name: rust-runtime-performance
description: "Rust runtime performance: measure before changing, cut allocations and data movement, bound async concurrency, and treat tuning as profiled experiments. Use when a Rust program is slow, memory-heavy or stalls under load, not when the build is slow."
user-invocable: false
metadata:
  version: "1.0.0"
category: agent-reference
effort: low
---
# Rust Runtime Performance

How fast the built program runs. Build speed is `rust-build-performance`;
gate and delivery process is `rust-delivery-workflow`. Order of work: measure,
fix the algorithm, remove allocation and copying, bound concurrency, then tune.

## 1. Measure First

- **State the objective**: latency (p50/p99), throughput, peak memory, or
  startup. A change that helps one often hurts another.
- **Release profile only.** Debug builds mislead by an order of magnitude. Keep
  debug symbols (`debug = "line-tables-only"` under a bench or profiling
  profile) so profiles name functions.
- **Profile before editing.** Use a sampling profiler (`cargo flamegraph`,
  `perf`, Instruments on macOS) for CPU, an allocation profiler (`dhat`,
  heaptrack) for memory, `tokio-console` for async stalls.
- **Benchmark with a harness** (`criterion` or `divan`), with
  `std::hint::black_box` around inputs and outputs so the optimizer cannot
  delete the work. Use realistic input sizes and a warm and a cold case.
- **Compare like with like**: same input, build profile, features, machine
  state. Repeat runs and read the spread; a gain inside the noise is not a gain.
- **Keep a before/after record** (command, input, result) in the PR or issue.
  Absolute numbers are host-specific; report the ratio and the method.

## 2. Algorithm and Data Structure First

Fix `O(n^2)`, repeated lookups and N+1 I/O before micro-tuning. Check the
collection: `HashMap` for keyed lookup, `BTreeMap` for ordered iteration,
`Vec` plus sort or `binary_search` for small or read-mostly sets,
`VecDeque` for queues. Hoist work out of loops; batch I/O.

## 3. Allocation and Data Movement

- **Borrow, do not clone.** Take `&str`, `&[T]`, `impl AsRef<..>`; return
  borrowed data where the lifetime is simple. Use `Cow<'_, str>` when most
  paths borrow and a few must own.
- **Preallocate.** `Vec::with_capacity`, `String::with_capacity`,
  `HashMap::with_capacity` when the size is known or estimable. Reuse a buffer
  across loop iterations (`clear()` keeps capacity) instead of rebuilding it.
- **Share, do not copy, large immutable data**: `Arc<[T]>` / `Arc<str>` /
  `bytes::Bytes` (cheap slicing and clone) instead of `Vec<u8>` copies across
  tasks.
- **Stream, do not collect.** Chain iterators and write results out as they
  appear; `collect()` into a `Vec` only when the next step needs random access.
  Read large files with a buffered reader, not `read_to_string` of everything.
- **Avoid per-item `format!` and `to_string()`** in hot paths; write into one
  buffer with `write!`.
- **Small values on the stack.** Reach for `SmallVec` or `ArrayVec` only after
  a profile shows allocator time on short collections.
- **Serialization copies**: deserialize into borrowed types
  (`#[serde(borrow)]`) when the input buffer outlives the value.
- **Locks and contention**: shrink the critical section, shard a hot map, or
  hand ownership to one task and message it, rather than widening a lock.

## 4. Async Concurrency Must Be Bounded

- **Bound fan-out.** Spawning a task per input item is unbounded memory and
  load. Use a `Semaphore`, a `buffer_unordered(n)` stream, or a bounded
  `mpsc` channel so back-pressure reaches the producer. Pick `n` from the
  downstream limit (connections, file descriptors, rate limit), not a guess.
- **Never block a runtime worker.** CPU-heavy or blocking-API work
  (`std::fs`, heavy parsing, `std::thread::sleep`) goes to
  `tokio::task::spawn_blocking` or a dedicated pool, or the async scheduler
  stalls for every other task.
- **`spawn_blocking` is a pool, and it grows.** The blocking pool starts small
  and grows on demand up to a high default cap, with no back-pressure. A
  loop that calls `spawn_blocking` per item can create hundreds of threads and
  exhaust memory. Bound those calls with a semaphore and, for steady load,
  set `max_blocking_threads` on the runtime builder.
- **No `std::sync::Mutex` guard across `.await`.** The guard is not `Send`
  across the await in a spawned future and, even where it compiles, blocks a
  worker thread while the task is parked. Copy the data out and drop the guard
  before awaiting, use `tokio::sync::Mutex` when the lock must span an await,
  or restructure around message passing.
- **Cancellation safety.** A future dropped at an `.await` loses partial work;
  in a `select!` branch, keep state outside the future being cancelled.
- **Do not hold large buffers across awaits** in many tasks at once; that is
  where bounded concurrency saves memory.

## 5. Tune as Experiments, After Profiling

Change one thing, rerun the benchmark, keep it only if the result clears the
noise. Revert what does not help.

- `opt-level`, `codegen-units = 1`, `lto = "thin"` or `"fat"`, and
  `panic = "abort"` trade compile time and binary behavior for speed. They
  apply to the release profile only; measure each separately.
- Allocator swaps (`mimalloc`, `jemalloc`) help allocation-heavy workloads on
  some platforms; measure on the target platform and do not assume.
- `#[inline]` and `#[cold]` hints are last-resort tuning on a profiled
  hotspot. `unsafe` for speed needs a benchmark that justifies it and a safety
  comment that states the invariant.
- Parallelism (`rayon`) pays when per-item work outweighs scheduling cost;
  check with a benchmark at realistic sizes.
- Record the accepted change and its measured effect next to the code or in the
  PR, so a later reader can tell a tuned choice from an accident.

## 6. Do Not

- Optimize unmeasured code, or report a gain from a debug build.
- Trade correctness, error handling or safety for an unprofiled win.
- Leave benchmarks, profiling profiles or scratch measurements committed
  without a reason; ship the benchmark only if it guards a real regression.
