Breaking
- New public fields on structs that are not `#[non_exhaustive]`, so a struct
  literal outside this crate stops compiling: `ReviewResult::withheld_count`,
  `ReviewResult::withheld_by_reason`, `ReviewResult::verdict_status` and
  `mapreduce::ReducedReview::wiped_model_verdict` (#9188).
- `finding_hygiene::relax_verdict_if_evidence_wiped` now returns
  `Option<Verdict>`, the model's verdict when it relaxed one, instead of `()`
  (#9188).
- `citation_gate::GateReport::partial` is removed: no finding is kept with a
  partly unverified citation any more (#9188 B).
