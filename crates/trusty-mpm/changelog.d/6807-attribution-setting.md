Changed

- The commit trailer and PR footer now come from Claude Code's own `attribution`
  setting: `ensure_settings_defaults` seeds `attribution.commit` and
  `attribution.pr` into the tm-owned `settings.json`, and an operator-set
  `attribution` is never overwritten. The footer text is removed from the PM
  instruction package, the seeded `CLAUDE.md` stub, and the bundled skills, so
  no session pays for it on every turn. `tm pr open`'s body validator reads the
  same constant (#6807).
