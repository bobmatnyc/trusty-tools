Fixed

- A staged instruction-compression savings row is no longer stranded when the
  hook resolves a different session scope than the compile did. `tm hook` now
  sweeps `~/.trusty-mpm/usage/pending-savings/` instead of probing the one file
  name it could rebuild: every row whose compiled prompt still exists is claimed
  and appended to the ledger, and only a row whose compiled prompt is gone is
  discarded — after 12 hours, with its measurement logged. A hook that finds
  nothing staged re-measures the fold from the compiled prompt on disk, so the
  `💸` statusline segment is no longer blank for a session whose row was never
  staged (#7411).
