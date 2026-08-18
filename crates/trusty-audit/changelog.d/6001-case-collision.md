Fixed

- Two local checkouts whose basenames differ only by case — `/srv/a/Apex` and
  `/srv/b/apex` — are now refused as a `CollidingCheckouts` at both
  registration and `clone_all`, instead of silently landing in one checkout.
  On a case-insensitive, case-preserving filesystem (APFS's default, and the
  one this feature runs on) `repos/local/Apex` and `repos/local/apex` are ONE
  directory, but the derived name kept its case, so the second repository was
  reported as audited having actually read the first one's history (the
  #5896 wrong-corpus family). The collision comparison in both gates is now
  case-folded, unconditionally.
- `trusty-audit add repo`'s usability check for a local checkout now asks
  `--is-bare-repository`, `--is-shallow-repository`, and `--show-toplevel` in
  one `git rev-parse` invocation instead of two, tolerating the toplevel flag
  failing on its own for a bare repository.
