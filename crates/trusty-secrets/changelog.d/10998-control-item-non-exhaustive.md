Breaking

- `store::ControlItem` gains the `PushOnly` variant and is now `#[non_exhaustive]`. A `match` on it outside trusty-secrets needs a wildcard arm. Later control items are no longer a semver break (#10998).
