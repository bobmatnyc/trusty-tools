Added

- **New `scip_status` MCP tool wraps `analyze.scip_status`** — an MCP caller can now distinguish an index with no SCIP overlay ingested from one whose overlay carried zero symbols, the same distinction `GET /indexes/{id}/scip` gave HTTP callers in #5054. `extract_graph`'s `scip_overlay` flag already carried this through the JSON-RPC body since #6287; this tool adds the dedicated node/edge/ingested_at lookup MCP had no way to reach ([#5056](https://github.com/bobmatnyc/trusty-tools/issues/5056))
