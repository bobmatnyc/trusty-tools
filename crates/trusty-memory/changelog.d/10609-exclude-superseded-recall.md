Changed

- Default recall (`memory_recall`, `memory_recall_deep`, `memory_recall_all`, and the in-process `MemoryService` recall used by the chat tools) now leaves out superseded drawers, so a retired `fact_key` slot occupant no longer appears at a halved score with no marker ([#10609](https://github.com/bobmatnyc/trusty-tools/issues/10609), [ADR-0074](docs/adr/0074-exclude-superseded-drawers-from-default-recall.md), owner ruling D83)
  - Retire-on-write stamps a `superseded-by:<id>` tag on the incumbent in the same atomic commit, so a slot retiree is excluded from its own row even when the KG edge lookup fails or runs late
  - A supersession known only from a `superseded_by` KG edge is excluded when the lookup answers; when it fails or exceeds its budget the response carries `supersession_unverified: true`
  - New optional `include_superseded: true` on the three recall tools is the historical search: it returns the superseded drawers, each tagged `superseded: true` and `superseded_by`, below their replacement
  - Dream dedup merges no longer copy a retirement marker onto the surviving drawer
  - The in-memory drawer table now carries the marker as well as the stored row, so a running daemon excludes a retired occupant at once rather than after the palace reopens, and a later rewrite of the drawer (chunk split, dream merge) keeps it
  - `superseded-by:*` is a reserved tag namespace: writes (`memory_remember`, `memory_note`, `task_add` and the engine write pipeline) strip a caller-supplied one and log a WARN, and recall and `memory_list` no longer show the marker among a drawer's tags (a historical search reports it as `superseded_by`)
