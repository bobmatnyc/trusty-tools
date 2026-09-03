Added

- `tga backfill pm-effort` scores the complexity of every meaningful PM ticket
  into the new `fact_pm_effort` table (#3915) — the EFFORT tier of the
  Activity / Work / Effort model, above the `fact_pm_work` tier #3916 added.
  The v1 formula (`formula_version = "pm-effort-1"`) sums a base of 1.0 with
  five independently capped terms — child count, description length, comment
  count, status-transition count and story points — into a 1.0–50.0 score,
  bucketed LOW / MEDIUM / HIGH. Every weight, cap and boundary lives in one
  `core::pm_effort::thresholds` block, because issue #3915 marks them all "TBD,
  refine with product": a retune ships as a new `formula_version` string, never
  as an edit of the v1 values, so a stored score always names the weight set
  that produced it.
- Two guards the raw formula does not provide. A ticket of a decomposable type
  (epic, feature, initiative) younger than 7 days is recorded as
  `DEFERRED_RECENT` with a NULL score rather than a low one — an epic filed
  yesterday has no children because nobody has broken it down yet, not because
  it is simple. And only tickets `fact_pm_work` marks meaningful are scored at
  all: an excluded ticket gets no row, and one that later becomes excluded
  loses the row an earlier run wrote.
- Story points degrade rather than zero the score. They are 76% NULL across
  four per-project custom-field IDs on the source JIRA instance, so the term is
  simply dropped when absent, out of range, or unparseable, and the row's
  `inputs_present` column names which terms actually fired. The offline
  extractor reuses `JiraClient::get_story_point_field`'s discovery shape —
  match by field name first, then fall back to the known ID list — because one
  global lookup cannot cover four spellings.
