Fixed

- `symgraph::SymbolGraph` no longer resolves a callee against one global
  bare-name map, first write wins (#6170) — the same defect PR #6169 fixed in
  trusty-search's parallel graph. A definition is now keyed `<file>::<symbol>`,
  and a call becomes an edge only when one scope around the CALLER holds exactly
  one candidate: the caller's own file, then each ancestor directory, then the
  whole corpus. Two crates defining `run` is no longer grounds for an edge to
  either, and a Rust call no longer binds to a `.ts` method of the same name. In
  the reverse direction, a callee beside the caller now wins over a same-named
  definition that was merely registered first — `upsert` calling `write` used to
  land in whichever crate sorted earliest.

Added

- `SymbolGraph::resolve_symbol` returns `SymbolMatch::{Unique, Ambiguous,
  NotFound}` for a caller-supplied name, so a consumer anchoring a trace can
  report which definition it took and what else matched. `callers_of`,
  `callees_of` and `context_for` anchor through the same resolution: a
  `<path>::<symbol>` name now anchors instead of missing, and a bare name that
  several definitions answer to picks the most-connected one rather than the
  first registered.
