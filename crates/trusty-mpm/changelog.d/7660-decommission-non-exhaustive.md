Breaking
- The library structs `DecommissionReport`, `DecommissionResponse` and `ManagedDecommissionOutcome` are now `#[non_exhaustive]`. Code outside the crate can no longer build them with a struct literal or match them without `..`; a field added later is no longer a breaking change. Refs #7660
- `DecommissionReport`, `DecommissionResponse` and `ManagedDecommissionOutcome` gain a public field, `workspace_kept_by_design: Option<String>`, carrying why a plain decommission kept a workspace tm never removes. Refs #7660
