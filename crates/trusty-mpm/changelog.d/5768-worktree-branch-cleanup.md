Documentation

- **`tm-workflow`'s Worktree Discipline section now points at BASE-AGENT for the post-merge cleanup rule instead of restating it**, and drops the unconditional `git branch -D` it previously recommended — verified that plain instruction leaves every local branch undeleted on this repo, since every merge here is a squash merge ([#5768](https://github.com/bobmatnyc/trusty-tools/pull/5768))
