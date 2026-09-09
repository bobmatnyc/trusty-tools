Added

- `tm doctor` gains a `hooks_build_tree_binary` check naming each offending
  file and the exact command inside it, including a `statusLine.command` that
  points into a build tree. `hooks_contamination` counts files, which says
  nothing useful when the entry is tm's own and the fault is the path inside it.
  The `statusLine` key is reported but not repaired here — the hooks writer does
  not own it, and the next managed launch upgrades it in place (#7262).
