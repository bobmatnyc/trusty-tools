Changed

- **Trusty Code owns `<project>/.trusty-code/` and `~/.trusty-code/` (#5426,
  epic #2892).** Every discovery site used to join the literal `.claude/`
  itself — agents, skills, plugins, `settings.json`, `CLAUDE.md` — which made
  another product's directory the source of truth for this one, left a clean
  project unable to install or run without a `.claude/` tree, and put nothing
  between the harness and a write INTO that directory. The new
  `trusty_code::paths` module is now the single resolver those call sites route
  through: `.trusty-code/` first, then `.claude/`, then `.open-mpm/`, and when
  none exists the resolved path names `.trusty-code/` anyway, so a project with
  no `.claude/` directory works end to end. `agent_loop::cadence` reads the same
  resolved `settings.json` as `mode` does, so `cadence_turns` and
  `max_overhead_fraction_pct` survive the move rather than silently reverting to
  their defaults on a `.trusty-code`-only project. A candidate that exists but
  cannot be read is skipped rather than fatal, and says so — a `warn` naming the
  path tried, and a line in `tcode paths show`. Writes are the other half and
  are deliberately a different function: `check_native_write_target` anchors
  `<project>/.trusty-code/` to the project before comparing anything against it,
  so neither a symlinked subdirectory nor a symlinked config ROOT can redirect a
  write outside the project. Private mutable state (transcripts, logs,
  telemetry, the workstream store) resolves through one place too and is created
  at mode `0700`, with an existing permissive `~/.trusty-code` tightened on the
  next run.
