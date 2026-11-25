Fixed

- The `list_branches_includes_current` test builds its own temporary git repo
  on a local `main` branch instead of reading the process working directory
  ([#9619](https://github.com/bobmatnyc/trusty-tools/issues/9619)). It failed
  in a detached-HEAD checkout or one with no `.git`, such as a copied build
  tree, although the code under test was correct. Test-only; no behaviour
  change.
