Breaking

- New public fields on structs that are not `#[non_exhaustive]`, so a struct
  literal outside this crate stops compiling: `Finding::citation_partial`,
  `ReviewResult::withheld_findings`, `GateReport::partial` and
  `GateReport::withheld_findings` (#8949).
- `GateReport` no longer derives `PartialEq` or `Eq`: its new
  `withheld_findings` holds `Finding`, which implements neither (#8949).
