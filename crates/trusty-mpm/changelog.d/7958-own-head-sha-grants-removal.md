Fixed

- The ADR-0057 worktree-removal guard now grants a worktree whose HEAD is exactly the head commit its own MERGED pull request carried, so a stale `@{upstream}` reporting the branch ahead no longer routes the decision through a merge-tree comparison that reports residue for a tree holding none; a HEAD that is not the pull request's head, an unresolvable HEAD, and a pull request GitHub named no head commit for all still deny (refs [#7958](https://github.com/bobmatnyc/trusty-tools/issues/7958))
