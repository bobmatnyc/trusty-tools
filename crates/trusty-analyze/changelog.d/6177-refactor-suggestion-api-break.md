Breaking

- `RefactorSuggestion` gains a public `region_kind` field, so an exhaustive
  struct literal built outside this crate no longer compiles.
- `core::refactor::analyze` (re-exported as `core::analyze_refactor`) takes an
  eighth parameter to carry the region kind through.
- Both are what move the version to 0.11.0 rather than 0.10.1; for a 0.x crate
  the breaking position is MINOR.
