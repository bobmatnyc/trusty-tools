Changed
- `Finding` has a new public field, `citation_correction: Option<CitationCorrection>`. `Finding` is not `#[non_exhaustive]`, so this is a semver-breaking change for any caller that builds a `Finding` with a struct literal; build it with `Finding::new` instead. The JSON form only gains an optional key (#8905).
