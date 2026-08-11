Added
- `tga::profile` — longitudinal contributor profiling: identity resolution
  (`ContributorSelector`), period-batch assembly (`assemble_period_batches`),
  category-stratified diff sampling (`sample_diffs_for_batches`), deterministic
  cross-period trend tagging (`synthesize_deterministic`), and JSON/Markdown
  rendering (`Reporter`). Ported from trusty-review so profiling lives with the
  data it reads (#5463, epic #5468). The model-written narrative (#5464) and
  GitHub issue publishing (#5465) are not part of this change — nothing here
  makes a network or model call.
