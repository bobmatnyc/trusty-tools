Fixed
- The history broadcast buffer is sized for the 1 Hz cadence. `EVENT_BUFFER` was
  128, chosen when a tick emitted one event every 5 s; a tick now emits two
  events every second, so a stalled browser was told it lagged after 64 s
  instead of 640 s. It is `HOST_HISTORY_CAPACITY * 2` — 1200 — which is two
  events per tick across the whole 10-minute window, and moves with the cadence
  rather than being a second number that can drift from it
  ([#6642](https://github.com/bobmatnyc/trusty-tools/issues/6642)).
- A panicking sampler tick no longer stops the history. The loop is one bare
  `tokio::spawn`, so a panic anywhere in a tick ended the task: the window
  froze, every open SSE stream stayed connected emitting nothing, and no log
  line said why. Each half of a tick now runs under `catch_unwind`, logs the
  panic payload at `error!`, and lets the next tick run — a panic in the service
  half leaves a gap in that series while the host graph keeps drawing. The
  loop's `JoinHandle` is kept and logs at `error!` if the task ever ends
  ([#6642](https://github.com/bobmatnyc/trusty-tools/issues/6642)).
