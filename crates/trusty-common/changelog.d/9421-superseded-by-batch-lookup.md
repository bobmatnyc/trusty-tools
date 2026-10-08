Added
- `KnowledgeGraph::superseded_by_many` returns the active `superseded_by` replacement for each drawer id in a batch, in one read transaction. Edges that were retracted, that point at something other than a drawer, or that point back at the same drawer are left out (#9421).
