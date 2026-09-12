Changed

- `tm agent show`'s tests build their `AgentMetadata` fixture from
  `AgentMetadata::default()` and assign fields, because the struct is
  `#[non_exhaustive]` from trusty-agents-common 0.8.0 on and a consumer crate
  can no longer write a struct literal for it. No behavior change (#7683).
