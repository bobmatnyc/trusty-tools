Fixed

- A reindex progress stream opened while a reindex was running could lose an
  event or deliver one twice. `ReindexProgress::push` appended to the replay
  buffer under a lock and broadcast after releasing it, while a stream opened by
  snapshotting the buffer under that lock and subscribing after releasing it — so
  an event appended after the snapshot and broadcast before the subscribe reached
  neither path, and one appended before the snapshot but broadcast after the
  subscribe reached both. A dashboard silently skipped a batch, or showed one
  twice, with no error anywhere. `push` now broadcasts while still holding the
  lock, and both transports open through one new
  `ReindexProgress::subscribe_with_replay`, which takes the replay buffer, the
  status, and the subscription under that same lock. Every event emitted through
  `push` or `push_terminal` now lands on exactly one path. Refs #6386.
- A stream opened while a reindex was FINISHING could end without its terminal
  frame. The six terminal transitions in the reindex runner stored the status and
  pushed the terminal event as two separate unlocked steps, and on the `Complete`
  path an RSS poll, a git subprocess, a marker-file write and two `RwLock` writes
  sat between them. A stream opening in that window read a terminal status while
  the replay buffer still lacked the terminal event, and both transports stop
  reading the live channel once the status is terminal — so the stream ended
  silently and the client waited on a completion that had already happened. A new
  `ReindexProgress::push_terminal` stores the status and emits the event under one
  hold of the replay-buffer lock, and every terminal transition routes through it.
  `ReindexTerminationGuard::drop` remains the one emission outside that rule,
  because `Drop` cannot await the lock. Refs #6386.
- `GET /indexes/{id}/reindex/stream` (SSE) and `search.index.reindex.stream`
  (Unix socket) shared the bug because each had its own copy of the two-step
  open. Both now call the one method, so the two transports cannot drift apart
  again. Neither route's observable frame sequence changes for a client that was
  not hitting the race. Refs #6386.
