Fixed

- `trusty-analyze` is no longer routed to the `x86_64-linux-al2023` /
  `aarch64-linux-al2023` assets on a below-glibc-floor host. It stopped
  bundling ONNX Runtime when its unused neural embedder was removed, so the
  release workflow no longer publishes an AL2023 variant for it and the old
  routing would have 404'd on an asset that is never built (#5067)
