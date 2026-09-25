Changed

- The `version-control` agent never switches branches, stashes or runs
  `reset --hard` in a main checkout; it publishes a branch with
  `git push origin <branch>`, which needs no checkout. Refs #8572.
