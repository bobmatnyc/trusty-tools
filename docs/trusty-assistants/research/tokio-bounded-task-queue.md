# Tokio Bounded Async Task Queue Patterns

**Date**: 2026-04-26
**Topic**: tokio::sync::mpsc, Semaphore, JoinSet patterns for bounded concurrent task queues
**Status**: Reference briefing

---

## 1. Bounded mpsc Channel

```rust
let (tx, mut rx) = tokio::sync::mpsc::channel::<Task>(capacity);
```

- `capacity` sets the buffer size (number of items buffered before backpressure kicks in).
- `send(&self, value: T) -> Result<(), SendError<T>>` is **async** — awaits when the buffer is full (true backpressure).
- `try_send(value: T) -> Result<(), TrySendError<T>>` returns immediately:
  - `TrySendError::Full(v)` — buffer at capacity, item not sent.
  - `TrySendError::Closed(v)` — all receivers dropped.
- Clone `Sender` for multiple producers; each clone keeps the channel open.

### Drain Pattern

When **all** `Sender` clones are dropped, the channel closes. `recv()` returns `None` only after the channel is closed AND the buffer is fully drained:

```rust
while let Some(task) = rx.recv().await {
    // process each task
}
// None returned: channel closed + buffer empty
```

---

## 2. Semaphore for Concurrency Limiting

```rust
let sem = Arc::new(tokio::sync::Semaphore::new(max_concurrent));

// Before spawning a task:
let permit = Arc::clone(&sem).acquire_owned().await.unwrap();
tokio::spawn(async move {
    let _permit = permit; // dropped at end of task, releasing the slot
    do_work().await;
});
```

Key points:
- `acquire_owned()` takes ownership of an `Arc<Semaphore>` and returns `OwnedSemaphorePermit`.
- `OwnedSemaphorePermit` is `Send + 'static` — safe to move into `tokio::spawn`.
- `Arc::clone(&sem)` **before** calling `acquire_owned()` is required (method consumes the `Arc`).
- The permit releases automatically on drop (RAII).
- `Semaphore::close()` wakes all pending `acquire*` calls with `Err` — useful for shutdown.

---

## 3. JoinSet for Collecting Task Results

```rust
let mut set: tokio::task::JoinSet<Result<Output, MyError>> = JoinSet::new();

set.spawn(async move { compute().await });

while let Some(join_result) = set.join_next().await {
    match join_result {
        Ok(Ok(val))  => handle_success(val),
        Ok(Err(e))   => handle_app_error(e),
        Err(join_err) => eprintln!("task panicked or cancelled: {join_err}"),
    }
}
```

- `join_next()` returns `None` when the set is empty (all tasks complete).
- Outer `Err` = `JoinError` (panic or cancellation).
- Inner `Err` = your application error type.
- **Dropping `JoinSet` cancels all running tasks** — hold it alive until drain completes.

---

## 4. Composing: Bounded Queue + Semaphore + JoinSet

```rust
use std::sync::Arc;
use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinSet;

async fn run_bounded_queue(capacity: usize, max_concurrent: usize) {
    let (tx, mut rx) = mpsc::channel::<Task>(capacity);
    let sem = Arc::new(Semaphore::new(max_concurrent));
    let mut set = JoinSet::new();

    // Producers: tx.send(task).await — backpressure at `capacity`
    // Drop all tx clones when work is enqueued.

    // Consumer loop: drain channel, respect concurrency limit
    while let Some(task) = rx.recv().await {
        let permit = Arc::clone(&sem).acquire_owned().await.unwrap();
        set.spawn(async move {
            let _permit = permit;
            task.execute().await
        });
    }

    // All senders dropped + buffer drained. Now drain JoinSet.
    while let Some(res) = set.join_next().await {
        // handle result
        let _ = res;
    }
}
```

---

## 5. Gotchas Summary

| Concern | Detail |
|---|---|
| `try_send` vs `send` | `try_send` never blocks — use for load shedding. `send` provides real backpressure. |
| `acquire_owned` requires `Arc` | Plain `&Semaphore` is not `'static` and won't cross `spawn` boundaries. |
| Semaphore `close()` | Makes all pending `acquire*` return `Err(AcquireError)`. Use as shutdown signal. |
| JoinSet drop cancels tasks | Hold `JoinSet` alive until fully drained if you need all results. |
| Channel close vs empty | `recv()` yields remaining buffered items before returning `None`. Never returns `None` while items remain. |
| `unwrap()` on acquire | Safe unless `Semaphore::close()` is called; use `?` in real code for shutdown paths. |

---

## References

- [tokio::sync::mpsc](https://docs.rs/tokio/latest/tokio/sync/mpsc/index.html)
- [tokio::sync::Semaphore](https://docs.rs/tokio/latest/tokio/sync/struct.Semaphore.html)
- [tokio::task::JoinSet](https://docs.rs/tokio/latest/tokio/task/struct.JoinSet.html)
- [OwnedSemaphorePermit](https://docs.rs/tokio/latest/tokio/sync/struct.OwnedSemaphorePermit.html)
