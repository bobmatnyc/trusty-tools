Fixed

- HTTP recall fuses the BM25 lexical lane instead of answering vector-only (closes [#5036](https://github.com/bobmatnyc/trusty-tools/issues/5036))
  - `GET /api/v1/palaces/{slug}/recall` — the route the `UserPromptSubmit` hook calls for every prompt — ran the vector lane and nothing else; the lexical lane had existed since #156 but was wired only into the MCP tool handler
  - `memory_recall_deep` had the same gap on the MCP side and now fuses too, so a query no longer answers differently depending only on `deep`
  - fusion can now PROMOTE a drawer only the lexical lane found. It previously only boosted drawers the vector lane had already returned, so a drawer dense retrieval missed stayed missed — which is the case a lexical lane exists to cover
  - a promoted lexical hit is scored on the vector lane's 0..1 band, as its share of the lexical lane's best hit, so `prompt_context`'s relevance floor (0.35, #5037) can judge it. A rank-scaled RRF score tops out near 0.033 and would have been filtered out of the injection immediately
  - BM25 search and live index writes now use the socket belonging to the palace being queried. Both used `AppState::bm25_client`, which is built once against the DEFAULT palace, so a search for palace X read a corpus the backfill never wrote to and a write for palace X landed in the default palace's corpus
  - the lexical lane runs concurrently with the vector lane, keeping the daemon round trip off the critical path
  - with `TRUSTY_BM25_DAEMON` unset — every shipped deployment today, per ADR-0031 — recall is byte-identical to before
