Fixed
- A managed relaunch now finds the session's stored conversation.
  `encode_project_dir` folded only `/` to `-`, but Claude Code folds every
  character outside `[A-Za-z0-9]`, so a workspace under `.worktrees/` encoded to
  `…-.worktrees-<id>` while the directory Claude Code really creates is
  `…--worktrees-<id>`. Because every tm-provisioned workspace sits under
  `.worktrees/` or `.claude/worktrees/`, `session_id_exists` returned false for
  every managed session: `spawn_resume` dropped `--resume <id>` and started a
  fresh conversation each time, and the SessionStart correlation
  ([#4337](https://github.com/bobmatnyc/trusty-tools/issues/4337)) judged the
  stored id stale and overwrote it on every launch. This also made the
  [#6765](https://github.com/bobmatnyc/trusty-tools/issues/6765) relaunch fix
  unreachable in practice — that change resolves the target explicitly via
  `--resume <id>`, and the id never resolved
  ([#6777](https://github.com/bobmatnyc/trusty-tools/issues/6777)).
- The encoder now reproduces Claude Code's `sanitizePath` in full: it folds the
  whole `[^A-Za-z0-9]` class, counts UTF-16 code units (one astral character
  yields two dashes), and truncates past 200 characters to `<200 chars>-<base36
  of the int32 path hash>`. tm's own managed worktree paths already reach 188
  characters, so the truncation branch is reachable, not theoretical.
