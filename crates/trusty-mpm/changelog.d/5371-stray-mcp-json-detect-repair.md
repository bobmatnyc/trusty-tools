Added

- `tm doctor` gained a `stray_mcp_json` check for `.mcp.json` files sitting
  ABOVE a workspace or in a temp root. Claude Code discovers `.mcp.json` by
  walking UP from a session's cwd, so one written above real projects supplies
  the MCP servers of every session started beneath it — agent scratchpads under
  `/tmp` included — with nothing in the project to point at. The scan is
  bounded to the workspace's strict ancestors up to the home directory, plus
  `$TMPDIR` and `/tmp`; never the filesystem root, never a recursive descent
  (PR #5371).
- A provenance ledger at `~/.trusty-mpm/mcp-json-provenance.json` records every
  `.mcp.json` tm writes, with a checksum of the bytes written. It is what lets
  a repair tell tm's own output from a file the operator wrote by hand — a
  distinction the file's contents cannot supply, since an operator may register
  the same `trusty-*` servers themselves (PR #5371).
- `tm doctor --fix` now quarantines stray `.mcp.json` files the ledger proves
  tm wrote and nobody has edited since, and `tm doctor --quarantine-mcp <PATH>`
  quarantines one the operator names explicitly. Both are DRY RUN until
  `--yes`. Neither deletes: the file is RENAMED to
  `.mcp.json.quarantined-<epoch>` beside itself, which is what stops the
  upward walk finding it while keeping every byte recoverable. Unattributed
  files, files edited after tm wrote them, symlinks, an unreadable ledger, and
  the workspace's own `.mcp.json` are each REFUSED with the reason shown
  (PR #5371).
