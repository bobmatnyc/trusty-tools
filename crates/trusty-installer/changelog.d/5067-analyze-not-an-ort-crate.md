Fixed

- `trusty-analyze` is no longer routed to the `x86_64-linux-al2023` /
  `aarch64-linux-al2023` assets on a below-glibc-floor host. It stopped
  bundling ONNX Runtime when its unused neural embedder was removed, so the
  release workflow no longer publishes an AL2023 variant for it and the old
  routing would have 404'd on an asset that is never built (#5067)
- **Breaking — version bumped 0.5.1 → 0.6.0 in this PR.** The public const
  `download::glibc::ORT_CRATES` narrows from `[&str; 2]` to `[&str; 1]`. Array
  length is part of the type, so any downstream binding it as `[&str; 2]`
  stops compiling, and under Cargo's 0.x rules a break takes the MINOR
  position. **`cargo semver-checks` does NOT catch this shape** — it has no
  lint for a `const`'s type changing and reported `196 checks: 196 pass, 58
  skip / no semver update required` against the 0.5.0 baseline. Read that
  green as absence of coverage, not as evidence of compatibility; the bump was
  made deliberately rather than left for the releaser to notice, because a
  fragment missed at cut time is exactly how #4088 turned into a yank
  (#5067, #4088)
