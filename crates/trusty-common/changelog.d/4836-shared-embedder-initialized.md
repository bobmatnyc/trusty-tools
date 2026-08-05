Added

- `retrieval::shared_embedder_initialized()` reports whether the process-wide embedder cell is live, without triggering a cold init ([#4836](https://github.com/bobmatnyc/trusty-tools/issues/4836))
  - lets a caller distinguish "the embedder is genuinely still warming" from "a startup flag was never cleared", instead of degrading on a stale signal
