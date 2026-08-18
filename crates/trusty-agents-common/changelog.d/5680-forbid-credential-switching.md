Added

- `BASE-AGENT.md` and `version-control.md` now forbid switching to a different
  `gh` account, token, or credential to obtain a permission the active one
  lacks; the agent reports the block to the PM instead. `version-control.md`
  also names the response to a `BEHIND` block with green CI —
  `gh pr update-branch`, or merge the head that is already green. A
  PM-relayed authorization to admin-merge is unchanged and still honored
  (#5680).
