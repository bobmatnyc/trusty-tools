Added

- **Progressive (seed + expand) loading for the palace knowledge-graph view
  (issue #4670).** The view used to fetch the entire graph in one call and lay
  it out with a hand-rolled O(n²) force simulation explicitly budgeted for
  "<500 triples"; the `trusty-tools` palace is now 8,266 triples / 9,311 nodes,
  16.5x that budget. Two new read endpoints back a bounded first paint plus
  click-to-expand:
  - `GET /api/v1/palaces/{id}/kg/graph/seed?limit=N` — the `N` highest-degree
    nodes and the edges among them, computed over the already-resident
    `petgraph` adjacency (no new storage, no new dependency). `limit` defaults
    to 75 and is clamped to `[1, 200]`. Measured 5.9 ms / 7.2 KB against the
    live 8,266-triple palace, versus 11.5 ms / 1.06 MB for the full graph.
  - `GET /api/v1/palaces/{id}/kg/graph/neighbors?node=X&direction=in|out|both&max_hops=N`
    — direction-aware, hop-bounded expansion around one node. `max_hops`
    defaults to 1 and is clamped to `[1, 4]`; an unparseable `direction` is a
    400. Parameter names and clamping mirror `trusty-search`'s
    `graph_neighbors_handler` so the two crates share one traversal vocabulary.
    Measured 0.5–0.8 ms on the live palace. **`direction=in` is the first HTTP
    route that can reach a node's incoming edges at all** — `GET /kg?subject=X`
    is a subject prefix scan and never reads the object side, so on the live
    palace the highest-degree node (48 edges, all inbound) returned *nothing*
    through the old route.
  - The graph view now loads the seed on mount, merges expansion results
    deduplicated by node id and by `(subject, predicate, object)`, and pins
    existing node positions during re-layout so an expansion grows outward
    instead of reshuffling the canvas. Nodes are sized by degree and carry a
    dashed halo when they still have unfetched edges. Full-graph load remains
    available as an explicit, size-warned opt-in.
