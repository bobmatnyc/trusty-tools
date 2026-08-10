Fixed

- The MCP `INDEX_NOT_READY` payload gained `next_steps.discover`, pointing at `list_indexes`, and `fallback_scope`, which names the circular-advice trap explicitly. `suggested_fallback: ["grep", "find"]` was previously the only actionable field; an agent read "grep" as trusty-search's own index-backed `grep` tool, which reports the same failure under the same session pin (#5213).
- An unresolvable `index_id` on `search`, the per-lane search tools, and every index-management tool now errors with a message naming `list_indexes` rather than the bare "missing required string field: index_id" (#5213).
- `search_all`'s tool description no longer contradicts its `index_id` parameter description. Both now state the actual three-tier precedence — explicit id, then the session pin (#1373), then cross-project fan-out only in an unpinned session — and the stale "issue #10" reference is gone (#5213).
