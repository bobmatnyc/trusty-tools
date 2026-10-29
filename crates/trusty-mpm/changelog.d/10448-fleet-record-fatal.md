Fixed
- `tm fleet init` exits 1 when it starts the Architect but cannot write its launch record. The error names the record path and the OS error, and `init` kills the tmux session it created, so no Architect runs unbound. Before, `init` warned and exited 0, and the unbound Architect had every trust-anchor write denied (#10448).
- `tm fleet relaunch` stops the new `claude` when its launch record cannot be written, and exits 1 with an error that names the record path and the OS error. Before, it exited 1 but left the unbound `claude` running (#10448).
- `tm fleet status` no longer reports `complete` while its `binding` row is `UNBOUND`. It exits 1 and names `tm fleet relaunch` to rebind the Architect. `complete` in `--json` follows the same rule (#10448).
