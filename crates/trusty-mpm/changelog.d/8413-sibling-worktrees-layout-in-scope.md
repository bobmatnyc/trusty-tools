Fixed
- The ADR-0057 `git worktree remove` scope check recognises the harness's `<repo>-worktrees/<tree>` sibling layout (#8413). A `version-control` removal of a linked worktree there now reaches the clean/pushed/merged/owner re-checks instead of being refused at `worktree-scope`. A main checkout that merely sits under a `*-worktrees` directory is still refused.
