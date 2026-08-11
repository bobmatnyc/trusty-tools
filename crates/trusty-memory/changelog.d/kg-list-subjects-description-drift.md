Fixed

- **`kg_list_subjects`' tool description told callers two things that stopped
  being true.** It claimed a `kg_query` miss "returns the same empty result as
  an empty graph, so a guess tells you nothing" — but since
  [#5385](https://github.com/bobmatnyc/trusty-tools/issues/5385) a miss carries
  `graph_state: subject_not_found` or `graph_empty` plus a hint. It also
  claimed `truncated: true` means "the page filled to `limit` and more subjects
  may exist" — but since
  [#4810](https://github.com/bobmatnyc/trusty-tools/issues/4810) the handler
  over-fetches one row, so `truncated` is set only when a further subject was
  actually seen, and a page that exactly fills `limit` reports `false`.
