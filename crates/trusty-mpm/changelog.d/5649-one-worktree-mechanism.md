Fixed

- `tm-workflow` and `tm-delegation-patterns` now name harness-managed
  `isolation: "worktree"` as the only sanctioned isolation mechanism for a
  file-mutating subagent dispatch, and state that the PM serializes when
  isolation is unavailable rather than hand-rolling `git worktree add` into a
  prompt. The old "name the exact worktree path" wording led PMs into a
  hand-rolled worktree the `tm hook --pm-guard` guard cannot see, which then
  denied the next dispatch for a collision that did not exist (#5649).
- The shared-worktree deny message now offers the serialize fallback alongside
  `isolation: "worktree"`, and says the guard reads the declared parameter and
  never the prompt (#5649).
