Documentation

- `version-control`'s Safety Rules now state that `git status --porcelain`
  must read empty before any push-gating check runs — a push ships the
  committed ref, not the working tree, so an edit the gate saw but never
  committed never reaches CI
  (refs [#7739](https://github.com/bobmatnyc/trusty-tools/issues/7739))
