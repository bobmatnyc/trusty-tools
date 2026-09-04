Changed

- The commit trailer and PR footer now come from Claude Code's own `attribution`
  setting instead of instruction prose. tm seeds `attribution.commit` and
  `attribution.pr` into two tiers, because they reach different launches:
  `ensure_settings_defaults` writes the tm-owned `CLAUDE_CONFIG_DIR` copy, which
  only the `claude` child tm spawns can see, and `write_output_style` writes the
  project-tier `.claude/settings.json`, which any `claude` launched in the
  project reads. Both seed absent-only, so an operator-set `attribution`
  survives. Setting both keys leaves neither on a default: per the settings
  reference, Claude Code "uses its default text for whichever of the two you
  left unset". `attribution` deprecated `includeCoAuthoredBy` in Claude Code
  v2.0.62; developed against 2.1.260.
- The footer text is removed from the PM instruction package, the seeded
  `CLAUDE.md` stub, and the bundled skills, so no session pays for it on every
  turn. `tm pr open`'s body validator reads the same constant (#6807).
