Added

- `GET /health` names the indexes behind `warmboot_summary.indexes_stage_failed`
  in a new top-level `indexes_stage_failed_ids` array (#6688). Until now the
  response carried counts only, so a consumer reading `indexes_stage_failed: 1`
  on a 41-index daemon had to poll `GET /indexes/:id/status` for every
  registration to find the broken one. The ids come from the same
  `IndexStages::any_failed()` predicate, in the same registry scan, as the
  counter, so the two can never disagree; they are sorted, because the registry
  iterates arbitrarily. The field is optional and omitted entirely when nothing
  is failing — every existing counter keeps its exact shape, and a consumer
  built against an older daemon needs no change. A consumer that mirrors the
  field must spell it `Option<Vec<String>>` or add `#[serde(default)]`; a bare
  `Vec<String>` hard-fails against a daemon that omits it.
