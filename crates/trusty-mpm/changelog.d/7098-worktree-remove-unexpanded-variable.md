Fixed

- `tm hook --pm-guard` refuses a `git worktree remove` whose path still carries an unexpanded shell variable at the ADR-0057 scope gate, naming the variable and quoting the token as written (#7098). `git -C $MAIN worktree remove $MAIN/…` used to join `$MAIN` twice and reach the clean-tree re-check, which then reported `git status --porcelain` failing for `<repo>/$MAIN/$MAIN/.claude/worktrees/…` — a directory the command never named. The removal was refused before and is refused now; what changed is that the refusal says what the guard could not establish.
