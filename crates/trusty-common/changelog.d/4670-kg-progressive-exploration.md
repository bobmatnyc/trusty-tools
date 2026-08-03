Added

- **`KnowledgeGraph::top_degree_subgraph` and `KnowledgeGraph::expand_neighbors`
  (issue #4670).** Two progressive-exploration primitives over the already-
  resident `petgraph::StableGraph`, backing the palace graph view's bounded
  first paint and click-to-expand. `top_degree_subgraph(limit)` returns the
  highest-degree entities (ties broken by name, so repeated calls are
  byte-identical) plus the induced edges among them, in O(V log V + E).
  `expand_neighbors(entity, direction, max_hops)` runs a direction-aware
  (`ExpandDirection::{In, Out, Both}`), hop-bounded BFS and returns the reached
  nodes — origin first, each carrying its graph-wide degree rather than its
  degree within the fragment — plus every traversed edge. Both emit edges as
  `Triple`s so callers need no second wire format. Neither touches disk.
