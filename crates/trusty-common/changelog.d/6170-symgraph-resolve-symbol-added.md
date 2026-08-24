Added

- `SymbolGraph::resolve_symbol` returns `SymbolMatch::{Unique, Ambiguous,
  NotFound}` for a caller-supplied name (#6170), so a consumer anchoring a trace
  can report which definition it took and what else matched instead of silently
  taking whichever was registered first.
