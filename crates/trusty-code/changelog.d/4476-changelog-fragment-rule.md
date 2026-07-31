Changed

- `BASE-AGENT.md` now directs agents to a per-PR changelog fragment file
  (`<package>/changelog.d/<number>-<slug>.md`) in preference to editing a shared
  `## [Unreleased]` section ([#4476](https://github.com/bobmatnyc/trusty-tools/issues/4476))
  - The fallback is conditioned on the project having no `changelog.d/` at all,
    rather than on the directory existing at that moment. A release used to
    delete the directory, which sent the very next PR back to editing
    `## [Unreleased]` and then blocked the following release.
