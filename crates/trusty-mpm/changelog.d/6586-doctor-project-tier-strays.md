Fixed
- `tm doctor`'s `skill_project_tier` check now reads the project tier from
  DISK. It intersected the bundled roster with `list_project_custom_stems`,
  which by design drops every stem the tier's
  `.trusty-mpm-skills-manifest.json` marks managed — and a copy the pre-#6602
  deploy left behind is managed by construction, so a project holding 51
  bundled copies reported `✅ … holds no bundled skill`. The ledger now decides
  only what the repair may remove, never what the check may report (#6586).
- `tm doctor --fix-skills` now removes those strays and drops them from the
  project's deploy ledger. It acts only on positive evidence: a copy the ledger
  records and whose bytes still match the recorded checksum. A bundled-named
  directory the ledger does not record is refused — it may be a project-custom
  skill written under a bundled name — and a recorded copy that was hand-edited
  is refused unless `--include-frozen`. Every removal is backed up whole,
  `references/` included, under
  `~/.trusty-mpm/backup-doctor-remediation-<timestamp>/project/<stem>/` and
  confirmed gone by re-reading disk. A project tier that resolves onto a tier
  bundled skills are deployed to is refused rather than swept, and
  `tm doctor --fix` still never deletes (#6586).
