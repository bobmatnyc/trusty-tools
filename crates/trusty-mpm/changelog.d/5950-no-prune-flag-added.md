Added

- `tm ls --no-prune` and `tm sessions ls --no-prune` make a listing a pure
  read: no dead-record decommission, no confirmation-marker write, no prune
  notice (refs [#5950](https://github.com/bobmatnyc/trusty-tools/issues/5950)).
  Auto-pruning every listing stays the default (#4702) and is now stated in
  both commands' `--help` rather than only in a doc comment. The prune's
  operator lines were already on stderr, so `--json` stdout is parseable JSON
  either way.
