Fixed

- The Non-Overridable Rules section's `trusty-search` guidance now matches the
  pinned-first, three-tier `index_id` resolution order trusty-search's own MCP
  descriptors have documented since #5213 (explicit `index_id` wins, then the
  session pin, then fan-out only when unpinned), and points agents at
  `list_indexes` before guessing an explicit id instead of only warning that a
  guess can 404 (#5337).
