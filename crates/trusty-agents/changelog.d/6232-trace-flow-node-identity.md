Fixed

- `trace_execution_flow` no longer mixes two same-named definitions in one
  trace (#6232). It anchored on the first node in insertion order and then
  re-queried callees by bare name, which resolves to the most-connected
  definition — so the reported root file and line could describe one function
  while the listed callees came from another. It now anchors through
  `SymbolGraph::resolve_symbol` and traverses by each node's `<file>::<symbol>`
  key.
- `trace_execution_flow` accepts a `<path>::<symbol>` entry point, which
  previously found nothing, and reports the other candidates under
  `ambiguous_with` when a bare name matches several definitions.
