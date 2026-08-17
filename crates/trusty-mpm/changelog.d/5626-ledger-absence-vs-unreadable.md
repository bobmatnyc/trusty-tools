Fixed

- `tm doctor`'s `asset_tier` check no longer prints
  `no tm-owned agent files outside the canonical tier … (scanned: <dir>)` for a
  directory whose scan failed (#5626, ADR-0045). A tier it cannot enumerate is
  reported as UNDETERMINED and downgrades an otherwise-clean run to `Warn`; the
  `scanned:` list now names only directories that were actually read. A
  confirmed shadowing still outranks it and keeps its `Fail`.
- Every `SkillManifest::load` call site now handles an unreadable ledger
  explicitly instead of proceeding on the empty default: the skill deploy, prune,
  retire, repair and adopt paths refuse to act, and the read-only probes
  (`skill_drift`, `skill_staleness`, `stale_skills`, `deploy_validate`,
  `doctor_scaffold_tracking`, `session_assets`, `update_check`) report the
  failure rather than "nothing is owned here".
