Fixed

- `tm hook --pm-guard` refuses the PM's `SendMessage` to an agent whose
  isolated worktree is gone, instead of letting Claude Code resume that agent
  in the main checkout, where it cannot commit. The refusal names the missing
  tree and its branch, and says to re-dispatch fresh with
  `isolation: "worktree"`. A recorded tree that cannot be confirmed to exist
  is treated as gone. A subagent's own `SendMessage` is never checked.
  (Refs #8004)
- The refusal's advice now distinguishes a tree the guard confirmed removed
  or missing (re-dispatch fresh) from one it could not verify at all — an
  unreadable or unparsable harness record — which may still be live, so the
  advice there is to retry or check `git worktree list` instead. (Refs #8004)
