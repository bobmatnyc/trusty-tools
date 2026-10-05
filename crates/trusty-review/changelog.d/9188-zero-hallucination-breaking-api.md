Breaking
- New public fields on structs that are not `#[non_exhaustive]`, so a struct
  literal outside this crate stops compiling: `ReviewResult::withheld_count`
  and `ReviewResult::withheld_by_reason` (#9188).
- `citation_gate::GateReport::partial` is removed: no finding is kept with a
  partly unverified citation any more (#9188 B).
