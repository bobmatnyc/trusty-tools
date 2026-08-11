Added

- New public `download::pinned` entry point installs prebuilt binaries at
  caller-specified EXACT versions and fails closed. `install_pinned_set` takes a
  set of `PinnedTool` pins, verifies each artifact against the published
  SHA-256 (and, optionally, a caller-supplied digest), verifies the downloaded
  binary itself reports the pinned version, and installs all tools or none —
  nothing reaches the install directory until every tool has verified. Every
  failure is a typed `PinnedError` naming what was pinned, what arrived, and
  that nothing was installed; `cargo install` is unreachable from this path
  (#5491)
- Version bumped 0.6.0 → 0.7.0 in this PR. The change is purely additive — a
  new module, new types, and `release::resolve_pinned_tag` — so a MINOR bump
  under Cargo's 0.x rules. `scripts/check_semver.sh --crate trusty-installer`
  confirms no breaking change against the published baseline (#5491)

Note

- `download::try_install_prebuilt` is deliberately UNCHANGED. It still resolves
  `latest` and still returns `Outcome::Fallback` to `cargo install` on any
  failure, because `install`/`upgrade` depend on that behaviour. The pinned path
  is a separate entry point rather than a change in its semantics (#5491)
