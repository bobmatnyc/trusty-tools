Changed
- The library structs `DecommissionReport`, `DecommissionResponse` and `ManagedDecommissionOutcome` are now `#[non_exhaustive]`, so a later field is not a breaking change. Code outside the crate can no longer build them with a struct literal. Refs #7660
