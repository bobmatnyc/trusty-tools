Documentation

- code-review-standards.md requires a file-size review finding to quote the project's own line-cap script in path-list mode, never a hand-rolled `grep`/`wc` count (Refs #7819).
- tm-ticketing.md's "Claim at dispatch" section states that filing a new issue is not itself a dispatch claim, so a filing-only brief no longer applies `status:in-progress` (Refs #7803).
- verification-before-completion's Regression Test Verification pattern requires an injected failure to reach the changed code path, not stop at an earlier guard (Refs #7440).
- tm-delegation-patterns.md's worktree-salvage section links the `git show <sha>:<path>` substitute for reading another worktree's committed state, since `git -C <other worktree>` is refused (Refs #7656).
