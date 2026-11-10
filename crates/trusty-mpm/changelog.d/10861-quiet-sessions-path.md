Fixed
- `tm architect quiet` reads the Architect's `local/quiet-sessions.txt` from any directory: without `ARCHITECT_PROJECT_DIR` it uses the Architect `tm fleet init` recorded, else `~/trusty-mpm-projects/architect`, instead of the current directory. A missing list still exits 1 and names that path (#10861).
- The seeded Python poller reads `<Architect dir>/local/quiet-sessions.txt`, the same file as `tm architect quiet` and the Rust poller, so a quieted session no longer raises idle alerts (#10861).
- Move an existing `<Architect dir>/quiet-sessions.txt` to `local/quiet-sessions.txt`; the poller reads the old path with a warning until then (#10861).
- `tm fleet init` creates `<Architect dir>/local/quiet-sessions.txt` holding only a comment header (no quiet sessions) when it is absent. An existing list is never rewritten, and the list is not created while an old `<Architect dir>/quiet-sessions.txt` still awaits the move; a create that fails is a FAILED step and init exits 1 (#10861).
