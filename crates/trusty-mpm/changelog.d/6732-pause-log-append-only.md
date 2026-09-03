Fixed
- `tm session pause` and the `session_context_pause` MCP tool no longer lose a
  `sessions-log.jsonl` line when two sessions pause at the same moment. The
  append these tools route through emitted the JSON line and its newline as two
  separate writes, which concurrent pauses could interleave into one unparseable
  line plus one blank line — the surviving record then read as a destructive
  rewrite of the log rather than an append
  ([#6732](https://github.com/bobmatnyc/trusty-tools/issues/6732)). The fix is
  in `trusty-common`; no trusty-mpm API changed.
