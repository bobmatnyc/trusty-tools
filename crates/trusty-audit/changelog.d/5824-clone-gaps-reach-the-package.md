Fixed

- A registered repository that failed to clone no longer vanishes from a
  one-shot `trusty-audit audit` run. Its failure was recorded only on the clone
  report, and because an unusable checkout is never selected, the sweep could
  not see it either — so the command exited 0 and handed back a package whose
  README said every repository was covered. The chain now folds the clone
  report's gaps into its own, which makes the exit status non-zero and puts the
  repository's name in the package's own `README.md` and `package.toml`.
