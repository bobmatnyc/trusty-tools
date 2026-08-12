Added

- `health` (JSON-RPC and `GET /health`) now carries an `incremental_index`
  object with `dropped_batches` and `seconds_since_last_drop`
  ([#2798](https://github.com/bobmatnyc/trusty-tools/issues/2798)). The
  write/edit tool executors hand every successful write to a bounded background
  index pool that drops a batch when a degraded trusty-search daemon has filled
  it; those drops were previously only a log line, so a sustained saturation
  episode reported exactly as healthy as no saturation at all. Both fields are
  additive — a client that reads only `server`/`version`/`status`/`pid`/`binding`
  is unaffected.
