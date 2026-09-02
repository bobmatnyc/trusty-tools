Fixed
- `tm doctor --fix-skills` now previews BOTH of its halves. The sweep half was
  gated on `--yes` and the redeploy half applied on the flag alone, so a bare
  `--fix-skills` printed `dry run — re-run with tm doctor --fix-skills --yes to
  apply` for the sweep and then rewrote three skill files and created
  `~/.trusty-mpm/backup-doctor-remediation-<timestamp>/` in the same invocation,
  with nothing in the output saying so. One command answering this crate's
  preview-by-default rule two different ways is what made the printed line
  untrue, so both halves now take the one mode the flag selects: on the bare flag
  the command writes nothing at all — no skill file, no ledger entry, no backup
  directory — and `tm doctor --fix-skills --yes` applies both. The redeploy's
  preview also names what it WOULD write and the flag that writes it; its summary
  gained a `planned` tally, which a preview previously reported as `skipped`.
  `tm doctor --fix` still previews the redeploy alone, `--fix --yes` still
  applies it, and neither ever runs the sweep, so the two commands stay distinct
  with or without `--yes` (#6620).
- The remediation pointers that name `tm doctor --fix-skills` as a repair now
  name `--yes` with it — the frozen-skill warning on managed-config deploy, and
  the `skill_staleness` per-tier remedies — since the bare command no longer
  writes (#6620).
