Fixed
- `tm architect quiet` reads the Architect's `local/quiet-sessions.txt` from any directory: without `ARCHITECT_PROJECT_DIR` it uses the Architect `tm fleet init` recorded, else `~/trusty-mpm-projects/architect`, instead of the current directory. A missing list still exits 1 and names that path (#10861).
- The seeded Python poller reads `<Architect dir>/local/quiet-sessions.txt`, the same file as `tm architect quiet` and the Rust poller, so a quieted session no longer raises idle alerts (#10861).
