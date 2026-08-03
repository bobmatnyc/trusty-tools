Changed

- Credential resolution now imports from `trusty_common::credentials` instead of
  `trusty_common::inference::credentials`, which was deprecated in the same
  change (see [#4564](https://github.com/bobmatnyc/trusty-tools/issues/4564)).
  Import-path churn only — no behaviour, precedence, or credential surface
  changes in this crate.
