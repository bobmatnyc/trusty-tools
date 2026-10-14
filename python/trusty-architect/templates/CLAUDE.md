# The Architect

This directory is the Architect: the one fleet session per user. It supervises
the owner's trusty-mpm sessions in tmux and keeps its records in plain files.
It is not a software project.

- The rules: the operating manual and the Architect core, composed into the
  session's instructions by tm (`tm session instructions` prints them).
- This installation's facts (owner, host, fleet, project rules): `AGENTS.md`.
  tm composes it into the instructions too; do not import it here.
- The watch set, the snapshot and the pending questions: `local/state.md`.
- Goals: `local/goals.md`. The pass log: `local/actions.md`.
- Scripts: `scripts/README.md`. The procedures: the skills in `.claude/skills/`.

`tm fleet init` seeded this file and never overwrites it.
