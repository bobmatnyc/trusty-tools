Changed

- The Knowledge Graph the assistant UI exposes (`GET /api/agents/:name/kg*`) now
  reads the assistant's OKG tree instead of trusty-memory's palace-scoped `kg_*`
  surface, and returns both halves of the graph — relationship triples and
  entity definitions. The envelope reports the tree it read and declares
  `source: "okg"`; the memory knowledge graph is a separate store and no longer
  reaches this surface (Refs #7430).
