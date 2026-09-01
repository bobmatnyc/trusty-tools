Fixed

- An agent-store worktree whose sentinel names no owner is refused whether that
  sentinel is unreadable OR absent. A missing sentinel is the absence of any
  attribution, not the absence of a claim, so resolving it toward "free" on a
  destructive path is what ADR-0045 forbids (#6561, #5661).
