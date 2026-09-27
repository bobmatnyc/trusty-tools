Documentation

- BASE-AGENT.md gains a "Field Techniques" section covering launchd state-line polling, per-commit rebase-empty prediction, home-wide search timeouts, and drift-guard target-repo confirmation via `git remote -v` (Refs #8529).
- BASE-AGENT.md's Handoff Protocol now states that edits on an already-checked-out branch land as commits on the branch actually checked out, and that untracked files missing from a branch diff are not deletions (Refs #8576).
