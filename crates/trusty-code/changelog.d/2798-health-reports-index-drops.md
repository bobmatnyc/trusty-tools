Added

- `health` (JSON-RPC and `GET /health`) now carries an `incremental_index`
  object with `dropped_batches`, `seconds_since_last_drop`,
  `truncated_batches`, and `seconds_since_last_truncation`
  ([#2798](https://github.com/bobmatnyc/trusty-tools/issues/2798)). The
  write/edit tool executors hand every successful write to a bounded background
  index pool, which loses work two ways once a degraded trusty-search daemon
  backs it up: it refuses a batch outright when full (a drop, nothing ran), or
  accepts one and then cuts it short at the 30s per-batch budget (a truncation —
  part of it landed and the files it had not reached are abandoned). Both were
  previously only a log line, so a sustained episode reported exactly as healthy
  as no saturation at all, and reporting only drops would still read `0`
  throughout an episode of repeated truncations. The two stay separate fields
  because they need different fixes. All four are additive — a client that reads
  only `server`/`version`/`status`/`pid`/`binding` is unaffected.
