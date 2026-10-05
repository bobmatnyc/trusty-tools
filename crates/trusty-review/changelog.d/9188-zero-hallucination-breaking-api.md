Breaking
- New public fields on structs that are not `#[non_exhaustive]`, so a struct
  literal outside this crate stops compiling: `ReviewResult::withheld_count`,
  `ReviewResult::withheld_by_reason` and `ReviewResult::verdict_status`, the
  additive fields of owner ruling 7t (#9188).
