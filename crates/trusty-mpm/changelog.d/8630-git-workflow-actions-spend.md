Added

- The bundled `git-workflow` skill gains a "GitHub Actions Spend" section: four checks to report before a workflow edit or a PR on a billed repo (default-branch-only push CI, PR-only cancel-in-progress, `timeout-minutes` on every job, change filters that keep required checks reporting), no runs spent for no new signal, and the "spending limit" billing failure as an owner blocker ([#8630](https://github.com/bobmatnyc/trusty-tools/issues/8630)).
