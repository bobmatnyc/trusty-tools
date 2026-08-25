Changed

- The bundled `git-workflow` skill's throwaway-worktree section now says that under trusty-mpm orchestration worktree removal is PM-executed and `tm hook --pm-guard` denies an agent's `git worktree remove`. This is a doctrine sync, not new trusty-code work: the file is byte-identical to `trusty-mpm`'s copy of the same skill, and leaving the paragraph out of one copy would leave two versions of one document disagreeing about a rule the guard now enforces (Refs #5791).
