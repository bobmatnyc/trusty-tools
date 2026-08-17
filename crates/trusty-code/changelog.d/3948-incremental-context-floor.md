Performance

- `session.get_context_budget` no longer rescans the multi-session
  `compression.jsonl` on every call; each session's working-context low-water
  mark is retained incrementally as measurements arrive. The durable JSONL is
  unchanged and remains the offline-history source (#3948).
