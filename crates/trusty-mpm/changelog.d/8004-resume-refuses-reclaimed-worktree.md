Fixed

- `tm hook --pm-guard` refuses the PM's `SendMessage` to an agent whose
  isolated worktree is gone, instead of letting Claude Code resume that agent
  in the main checkout, where it cannot commit. The refusal names the missing
  tree and its branch, and says to re-dispatch fresh with
  `isolation: "worktree"`. A recorded tree that cannot be confirmed to exist
  is treated as gone. A subagent's own `SendMessage` is never checked.
  (Refs #8004)
