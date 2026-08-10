Fixed

- The stray-`.mcp.json` quarantine compared paths lexically, so a caller
  holding a symlinked spelling of the workspace slipped past the guard that
  protects the project's own `.mcp.json` and the file was renamed away. Path
  comparisons now canonicalize both sides through one shared helper, which also
  fixes the scan's home ceiling — a symlinked home spelling never matched, and
  the walk ran past it to the depth cap (PR #5371).
- The sweep classified provenance during the scan and renamed later, so a file
  edited in that window was quarantined on a stale verdict, discarding the
  edit. The checksum is now re-read immediately before the rename and a file
  that changed is refused (PR #5371).
- The provenance ledger grew one entry per distinct `.mcp.json` path and never
  shed one. Above 512 entries it now drops records whose file no longer exists
  (PR #5371).
