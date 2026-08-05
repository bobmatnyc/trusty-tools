Added

- `memory_recall` and `memory_recall_deep` accept a `room` scope (ADR-0027 T7).
  The filter has worked in the retrieval layer since #3274, but the MCP schema
  carried only `palace`/`query`/`top_k`, so room-scoped recall was reachable
  only through `memory_list` or the HTTP `/recall` route. While the embedder is
  still warming, the lexical fallback lane is filtered too, so a room-scoped
  recall never returns another room's drawer because of daemon state.
