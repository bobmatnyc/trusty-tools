Fixed

- `tm sessions rename` (and the `tm ls` picker's `r<N> <new-name>`) now surfaces the daemon's own failure message instead of a bare `HTTP status server error (500 …)` (closes [#5001](https://github.com/bobmatnyc/trusty-tools/issues/5001))
  - the client called `error_for_status()`, which discards the response body — the daemon had always sent an actionable reason there, and it was thrown away before the operator ever saw it
  - the daemon also now logs a `warn!` when a rename fails with an unmapped error, so a 500 leaves a diagnosable trace
