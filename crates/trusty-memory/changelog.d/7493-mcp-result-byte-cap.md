Added

- MCP results fold to a 48 KiB serialized byte ceiling on the seven result-returning tools — `memory_recall`, `memory_recall_deep`, `memory_recall_all`, `memory_list`, `kg_query`, `chat_session_recall`, `list_prompt_facts` ([#7493](https://github.com/bobmatnyc/trusty-tools/issues/7493))
  - whole entries are dropped from the tail, never one cut mid-object, and the response carries `truncated`, `returned`, `withheld`, and a `truncation_notice` naming the knobs that fetch the rest
  - a recall's L0 identity and L1 essential drawers are never dropped; L2 hits go first
  - `max_bytes` overrides the ceiling up to 512 KiB, with the clamp reported in the response; `full: true` disables it. A non-integer `max_bytes` or a non-boolean `full` is rejected, not coerced
  - a response whose smallest foldable form still exceeds the ceiling ships anyway, under a notice that says so rather than one claiming it fits
