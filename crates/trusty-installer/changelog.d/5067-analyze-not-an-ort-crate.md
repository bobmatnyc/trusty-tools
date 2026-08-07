Fixed

- `trusty-analyze` is no longer routed to the `x86_64-linux-al2023` /
  `aarch64-linux-al2023` assets on a below-glibc-floor host. It stopped
  bundling ONNX Runtime when its unused neural embedder was removed, so the
  release workflow no longer publishes an AL2023 variant for it and the old
  routing would have 404'd on an asset that is never built (#5067)
- **Note for the releaser:** this narrows the public const
  `download::glibc::ORT_CRATES` from `[&str; 2]` to `[&str; 1]`. The array
  length is part of the type, so that is a breaking change for any downstream
  binding it as `[&str; 2]`. `cargo semver-checks` passes it (196 pass, 0 fail)
  — it has no lint for a `const`'s type changing — so the gate is green by
  coverage gap, not by compatibility. Judge the next version accordingly
  (#5067, #4088)
