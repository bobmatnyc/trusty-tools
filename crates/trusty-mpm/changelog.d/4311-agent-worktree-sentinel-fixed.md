Fixed
- ADR-0036's "Related Decisions" cited "DOC-66 §3's per-worktree sentinel extension". The content is in §1.3; §3 is "Refreshing the tm checkout" and has no sentinel content.
- `.trusty-mpm-worktree` is now gitignored. The daemon writes it INTO a worktree, so it surfaced there as an untracked file and tripped the clean-tree guard in `scripts/check-publish-ready.sh`.
