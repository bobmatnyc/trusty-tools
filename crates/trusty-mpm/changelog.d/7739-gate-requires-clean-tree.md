Documentation

- The `version-control` agent brief and the `tm-workflow` skill both state that `git status --porcelain` must read empty before any push-gating check runs, so a push ships the ref the gate proved rather than a working tree it never saw ([#7739](https://github.com/bobmatnyc/trusty-tools/issues/7739))
