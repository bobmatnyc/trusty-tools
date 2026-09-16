Fixed

- The ADR-0057 worktree-removal guard now resolves a detached-HEAD checkout by its commit instead of refusing it for having no branch name: a clean tree parked on the exact head a MERGED pull request was opened from is granted removal, and an unresolvable HEAD, an unanswered commit search, or a commit no merged pull request carries all still deny (refs [#7832](https://github.com/bobmatnyc/trusty-tools/issues/7832))
