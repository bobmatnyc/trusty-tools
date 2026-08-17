Changed
- `tctl install`'s human summary now reads the gating partition from
  `stable_set::gating_split` rather than re-deriving `required || !any_required`
  inline, so the footer and the `all_ok` verdict share one expression of the
  rule and cannot drift apart again (#5806).
