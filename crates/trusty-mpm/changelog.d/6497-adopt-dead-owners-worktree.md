Added

- `tm session adopt-worktree <path> --as <session>` transfers a worktree whose
  owning session or agent is provably dead to a live session, by rewriting the
  ownership sentinel. The daemon refuses unless the current owner is positively
  known to have ended and no live delegation is still working in the tree; an
  owner the delegation registry has merely never heard of is undeterminable and
  refuses too (ADR-0045). The verb is EXPLICIT rather than automatic — a design
  choice this change makes, since a tree that changes hands on its own is
  indistinguishable from one taken from a working agent (#6497).
