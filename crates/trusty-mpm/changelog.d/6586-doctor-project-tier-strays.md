Fixed
- `tm doctor`'s `skill_project_tier` check now reads the project tier from
  DISK. It intersected the bundled roster with `list_project_custom_stems`,
  which by design drops every stem the tier's
  `.trusty-mpm-skills-manifest.json` marks managed — and a copy the pre-#6602
  deploy left behind is managed by construction, so a project holding 51
  bundled copies reported `✅ … holds no bundled skill`. The ledger now decides
  only what the repair may remove, never what the check may report (#6586).
- `tm doctor --fix-skills` now removes those strays and drops them from the
  project's deploy ledger. Because it DELETES it follows this crate's rule for
  a write: the bare flag PREVIEWS the sweep and `tm doctor --fix-skills --yes`
  applies it. It acts only on positive evidence: a copy the ledger records
  whose whole subtree still matches what tm deployed — the same
  `skill_removal_verdict` the #5224 retirement sweep and the deselection prune
  use, so a file the operator added or edited anywhere under the directory
  stops the removal. `--include-frozen` does not override that: it promotes an
  overwrite of one file, not a whole-directory deletion. A bundled-named
  directory the ledger does not record is refused — it may be a project-custom
  skill written under a bundled name — and a bundled-named entry that is not a
  skill directory at all is now reported rather than skipped in silence. Every
  removal is backed up whole, `references/` included, under
  `~/.trusty-mpm/backup-doctor-remediation-<timestamp>/project/<stem>/` and
  confirmed gone by re-reading disk. The sweep runs BEFORE the redeploy, and
  both halves share one backup root. A project tier that is a SYMLINK, or whose
  canonical path resolves onto a tier bundled skills are deployed to, is
  refused rather than swept — the previous lexical `PathBuf` comparison over an
  `is_dir()` probe let a `.claude/skills` symlinked at `~/.claude/skills`
  through, and the sweep would have deleted the operator's live home-tier
  skills. `tm doctor --fix` still never deletes (#6586).
- A bare `tm doctor --fix-skills` now writes nothing at the project tier. The
  sweep ran as a dry run and printed "would remove", and the REDEPLOY half then
  rewrote those same copies from the bundled asset and re-stamped each ledger
  checksum — on the 51-stray project, 51 files and 51 backups written straight
  after the command said nothing would be. The redeploy now skips every stem the
  sweep planned or applied. Its own gating is unchanged and deliberate: it
  overwrites tm's own files, backs each one up, and has `tm doctor --fix` as its
  preview, so it still applies on the flag alone; only the deletion waits for
  `--yes` (#6586).
- A project tier that exists, permits the deploy-ledger lock, and cannot be
  LISTED is now one refusal naming the tier and the error. Both scanners treated
  an unreadable directory as an empty one, so the sweep produced no steps at all
  for a tier the `skill_project_tier` check reports as undetermined (#6586).
- A symlink anywhere under a stray now stops the removal. The backup copied the
  link's TARGET bytes as a plain file and `remove_dir_all` then unlinked the
  link, so the operator had no way back to it (#6586).
- The `skill_project_tier` check now counts the bundled-named entries that are
  not skill directories, which the sweep already reported as refusals. It said
  `it holds no bundled skill` about a tier `--fix-skills` then listed (#6586).
- `--fix-skills --help` no longer implies `--include-frozen` protects a
  hand-edited subtree indefinitely. It does not override the refusal within a
  run, but it overwrites the frozen file and re-stamps its checksum, so the
  subtree becomes removable on the next sweep (#6586).
