Fixed
- **Every memory-backed OKG store reported `palace_connected: false`, and the GUI's KG pane rendered permanently empty.** `GET /api/agents/:name/stores`, `…/knowledge` and the four `…/kg*` routes resolved `resolve_daemon_base_url("trusty-memory")`, which reads an `http_addr` file ADR-0032 stopped writing — so it was `None` on every machine and every request took the degraded arm with "daemon not discoverable", whether or not the daemon was up ([#6286](https://github.com/bobmatnyc/trusty-tools/issues/6286))
  - the palace probe is `memory.drawers_list` with `limit: 1` — the method its `GET …/drawers?limit=1` predecessor folded onto — and a not-found refusal is what marks the palace absent, the distinction the 404 status used to carry
  - the four KG reads map onto `memory.kg_subjects_with_counts`, `memory.kg_all`, `memory.kg_count` and the `kg_query` tool. `kg_query` answers `{subject, triples, …}` where the retired route answered a bare array, so `/kg?subject=` lifts `triples` — the one projection this proxy makes, named at the type rather than buried
- A palace that exists but cannot be opened still reads as a failure rather than an absence (#5592's distinction), now off the JSON-RPC code instead of the HTTP status

Changed
- `stores::resolve_store_statuses`, `api::server::agent_stores::stores_at`, `agent_knowledge::knowledge_at` and `agent_kg::kg_proxy_at` take the trusty-memory SOCKET (`Option<&Path>`) where they took a base URL. `agent_kg::KgRead` names a method and its params rather than a URL suffix and query pairs
