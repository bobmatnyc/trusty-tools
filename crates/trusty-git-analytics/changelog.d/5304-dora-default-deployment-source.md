Fixed

- `tga deployments collect` no longer fails with `unknown deployment_source ''` when `config.yaml` has no `dora:` block (#5304). `DoraConfig`'s derived `Default` zeroed `deployment_source`, and the `#[serde(default = "…")]` fallbacks only fire when a `dora:` mapping is actually present — so the `unwrap_or_default()` path bypassed them. `Default` is now hand-written through the same `default_*` functions, and an audit of a repo set without DORA configured no longer reports a failed stage.
