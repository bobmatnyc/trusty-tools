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

- `install_pinned_set` places the whole set with copy-then-commit: every binary
  of every tool is copied into the install directory under a hidden temporary
  name, and nothing is renamed to its final name until all copies succeed.
  Previously phase 2 placed tools one at a time, so a failure on tool 2 returned
  an error reading "nothing was installed" while tool 1's binary was already on
  disk (#5517)
- New `PinnedError::PlacementInterrupted` covers the one case where files do
  survive a failure — a commit rename failing after an earlier one succeeded. It
  lists every file left in the install directory and every crate they belong to,
  instead of reusing the `Io` variant's "nothing was installed" text. Every other
  variant now provably means no file was placed under its final name (#5517)
- A set whose tools install two binaries to the same path, and a destination
  already occupied by a directory, are both rejected before anything is copied
  (#5517)

Note

- Check 5 EXECUTES the downloaded binary to read its `--version`, so a pinned
  install runs a freshly downloaded, not-independently-signed artifact in the
  installer's own process and user context during an unattended install. The
  digest gating it is self-published by the same release pipeline that would be
  compromised in the attack this guards against. The module doc records this as
  an accepted trade-off: a mis-tagged or mis-built asset passes every URL- and
  digest-level check, and executing the binary is the only thing that catches it
  (#5517)
- `download::try_install_prebuilt` is deliberately UNCHANGED. It still resolves
  `latest` and still returns `Outcome::Fallback` to `cargo install` on any
  failure, because `install`/`upgrade` depend on that behaviour. The pinned path
  is a separate entry point rather than a change in its semantics (#5491)
