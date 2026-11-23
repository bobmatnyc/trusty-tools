Changed
- `stores::resolve_store_statuses`, `api::server::agent_stores::stores_at`, `agent_knowledge::knowledge_at` and `agent_kg::kg_proxy_at` take the trusty-memory SOCKET (`Option<&Path>`) where they took a base URL. `agent_kg::KgRead` names a method and its params rather than a URL suffix and query pairs
