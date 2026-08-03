Fixed

- **The palace graph view no longer presents a truncated graph as complete
  (issue #4670).** `GET /api/v1/palaces/{id}/kg/graph` capped `triples` at
  `KG_GRAPH_MAX_TRIPLES` (5,000) while `node_count` / `edge_count` /
  `community_count` in the same payload were computed over the FULL in-memory
  adjacency. On the live `trusty-tools` palace that meant the UI rendered 5,000
  triples under a badge reading "9,311 nodes", with nothing in the response
  indicating a partial view — and because `list_active` orders by `valid_from`
  DESC, the 3,266 dropped triples were silently the OLDEST ones. The payload now
  carries `returned_triple_count`, `active_triple_count`, and a derived
  `truncated` flag, and the graph view's header always states what is rendered
  versus what exists ("75 of 9,311 nodes shown — click a node to expand").
