Fixed

- `memory_recall` and `memory_recall_deep` no longer return the same drawers for every query (closes [#4836](https://github.com/bobmatnyc/trusty-tools/issues/4836))
  - the daemon readiness flag was written once, by the startup embedder warm-up; a single failed attempt pinned it at `Warming` for the daemon's whole life, and the MCP recall handlers read it to pick a degraded fallback that ignores the query. Resolving the embedder now clears the flag, and recall consults the embedder itself rather than trusting a flag that cannot be refreshed from the path that reads it.
  - `AppState` no longer carries a second embedder cell independent of `retrieval::shared_embedder()`. Startup latched readiness off one cell while every recall used the other, so the flag described an embedder the request path never touched — and the daemon loaded two ~90 MB ONNX sessions instead of one.
