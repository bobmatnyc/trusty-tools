Breaking

- `daemon::managed_routes::inproject::get_origin_url` returns
  `Result<Option<String>, String>` rather than `Option<String>`, so a git
  failure can no longer be mistaken for an absent remote (refs
  [#4734](https://github.com/bobmatnyc/trusty-tools/issues/4734)). A caller that
  wants the previous fail-open behaviour now spells it out with
  `.ok().flatten()`.
- `ColdStartError` gained an `OriginUnreadable` variant, so an exhaustive match
  over it needs a new arm.
