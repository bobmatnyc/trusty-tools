Fixed

- `symgraph::SymbolGraph` no longer resolves a callee against one global
  bare-name map, first write wins (#6170) — the same defect PR #6169 fixed in
  trusty-search's parallel graph. A definition is now keyed `<file>::<symbol>`,
  and a call becomes an edge only when one scope around the CALLER holds exactly
  one candidate: the caller's own file, then each ancestor directory, then the
  whole corpus. Two crates defining `run` is no longer grounds for an edge to
  either, and a call no longer binds across languages — `.ts` and `.tsx` count
  as one language, so twins in both keep each other ambiguous instead of one
  looking unique. In the reverse direction, a callee beside the caller now wins
  over a same-named definition that was merely registered first — `upsert`
  calling `write` used to land in whichever crate sorted earliest.
- A call no longer resolves to a symbol that cannot be called. The same-file
  shortcut used to answer before the callable check, so a `Calls` edge could
  land on a struct or an import sitting in the caller's own file; two symbols
  sharing one `<file>::<symbol>` key now resolve to neither rather than to
  whichever was inserted last.
- `callers_of`, `callees_of` and `context_for` anchor through that same
  resolution: `<path>::<symbol>` now anchors on the file it names — matched
  against each candidate's own file, so a partial path suffix works too — and a
  bare name that several definitions answer to picks the most-connected one
  rather than the first registered.
