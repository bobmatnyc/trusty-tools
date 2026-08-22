Changed
- BREAKING: `RepositoryEntry` (report::manifest) and `RepositoryReport`
  (report::model) each gain a public `crate_topology` field. Both are
  exhaustively constructible through the public API, so any external struct
  literal over either one stops compiling and must add the field (`None` keeps
  the previous behavior). This is why the crate goes to 0.23.0 rather than a
  patch release — for a `0.y.z` crate the breaking bump is the MINOR position.
