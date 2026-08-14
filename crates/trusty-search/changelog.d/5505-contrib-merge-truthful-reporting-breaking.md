Breaking

- `save_then_merge_contrib` returns `(Option<Arc<SymbolGraph>>,
  ContribMergeOutcome)` and `CodeIndexer::rebuild_symbol_graph_now` returns
  `ContribMergeOutcome`. Both were previously infallible-looking; callers that
  ignore the outcome are unaffected in behaviour, but this is a breaking change
  to the library API.
