Changed

- The guided flow and `trusty-audit run` now install the pinned tools they are
  missing instead of reporting them and stopping. The guided flow installs once
  repositories are chosen — the point it used to print "install the tools" — and
  `run` installs before the sweep's preflight. Both call the same all-or-none
  `trusty-installer` entry point `trusty-audit install` does, so a set that
  cannot be fully resolved installs nothing and the command fails; the #5454
  guarantee is reached earlier, not relaxed
  ([#5797](https://github.com/bobmatnyc/trusty-tools/issues/5797))
- A binary the client did not place — the `UNVERIFIED` row in `trusty-audit
  tools` — is reinstalled rather than kept. Nothing may claim a version for it,
  and an unknown version is the #5454 version-skew input; `tools/` is the
  client's own area under the work-dir root, so replacing it costs nothing
  ([#5797](https://github.com/bobmatnyc/trusty-tools/issues/5797))

