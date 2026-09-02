Added

- Session launch now prints one line per unclean asset kind, and nothing at all
  when every kind is clean. The three lines are `agents quarantined N (names)`,
  `skills stray N (names)` and `duplicates N (names)`. Two of those findings were
  already computed at launch and went only to `tracing`, which a terminal session
  does not show — the #4448 quarantine MOVES files in the operator's working tree
  and said so at `warn` level. A tier that cannot be listed, and a bundled roster
  that cannot be built, each produce an `UNDETERMINED` line rather than reading as
  clean (#6649).
- `tm doctor --fix-agents` sweeps bundled AGENT copies stranded at a project's own
  `.claude/agents/`, mirroring `--fix-skills`. It previews by default and removes
  only on `--yes`, and only a copy that tier's `.trusty-mpm-manifest.json` records
  as tm's, with a framework-owned origin, whose bytes still match the recorded
  checksum. A bundled-named DIRECTORY, an untracked file, a ledger entry the
  operator owns, and a copy hand-edited after deployment are each refused and
  reported. Every removal is backed up first; `tm doctor --fix` never runs it
  (#6649).
- `tm doctor` gains an `asset_duplicates` row: one asset name claimed by two
  entries in the SAME tier — `qa.md` beside a `qa/` directory, or `QA.md` beside
  `qa.md` — for both agents and skills. Only one of the two ever loads, and on a
  case-insensitive filesystem the two names are one file. `asset_tier` and
  `skill_project_tier` both compare one directory against another and cannot see
  this. Report-only, with no repair: tm cannot know which entry the operator meant
  to keep (#6649).
