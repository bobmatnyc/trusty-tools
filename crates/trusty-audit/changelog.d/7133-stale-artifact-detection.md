Added

- Each collected repository now records the `trusty-audit` version that
  collected it (`run::RepoRun::collected_by_version`), carried over unchanged
  when a resumed sweep skips re-collection. `trusty-audit package` compares
  that recorded version against the version assembling the package and, for
  every audited repository whose artifact is older or does not name a
  version at all (a legacy artifact predating this field), reports it by
  repository name, collected version and running version — in
  `package.toml`'s new `stale_artifacts` array and on the console — rather
  than packaging it silently. The repository is still packaged: this is a
  warning, not a refusal. New public items `run::RepoRun::collected_by_version`
  and `package::ReturnPackage::stale_artifacts`; no existing member, signature
  or struct shape changed (issue #7133).
