Fixed
- New managed sessions are named bare `tm-<project>`, with a `-02`, `-03`, … counter only on a duplicate, and a hint such as `tm-mpm-bugs-01` no longer doubles into `tm-tm-mpm-bugs-01-01`. The `tm ls` Ctrl-N preview and the picker's `n <name>` row show the new shape. Existing sessions keep their names (#10478).
