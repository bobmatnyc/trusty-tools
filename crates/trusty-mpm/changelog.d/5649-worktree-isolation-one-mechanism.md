Fixed

- `tm-workflow` and `tm-delegation-patterns` now ban telling a dispatched agent to hand-roll its own worktree in ANY wording, not only a literal `git worktree add` — the prose form ("work in a worktree of your own") recurred in a different project after the literal-command form was fixed, because `tm hook --pm-guard` cannot see either phrasing in a dispatch prompt (#5649).
