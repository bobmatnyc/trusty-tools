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
