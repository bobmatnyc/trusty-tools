Fixed

- `tm doctor`'s `legacy_sources` check no longer flags the directory `tm install`
  deploys skills into. The installer writes the bundled roster to the tm-managed
  `CLAUDE_CONFIG_DIR/skills` tier (#6586) and only user-custom skills to
  `~/.claude/skills`, so deleting the flagged copies now clears the warning
  instead of reproducing it on the next `tm install`.
- The `legacy_sources` remediation text says to delete the leftover
  `~/.claude/skills/tm-*` copies rather than to run `tm install`.
