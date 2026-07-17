# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---
## [0.2.2] — 2026-07-17

### Added

- new public `agents` module (`agents::builder`, `agents::deployer`, `agents::manifest`, `agents::frontmatter`): the `extends:`-inheritance agent composer, the checksum + atomic-write ownership manifest, and the deploy pipeline extracted from `trusty-mpm`'s binary crate for cross-crate reuse (refs #2892) (closes part of the #2892 extraction). `agent_manifest`'s error type is now a self-contained `ManifestError` (thiserror) so the shared crate carries no host-crate dependency. Additive only — no breaking changes to existing exports ([#2909](https://github.com/bobmatnyc/trusty-tools/pull/2909)) ([`bb947ea`](https://github.com/bobmatnyc/trusty-tools/commit/bb947ead9e220a37b8902b1190d261295c23538b))
- new public `skills` module (`skills::deployer`, `skills::manifest`, `skills::tiers`): the skills deploy/manifest/tiers machinery extracted from `trusty-mpm`'s binary crate for cross-crate reuse (refs #2892, #2818). Additive only — no breaking changes to existing exports ([#2916](https://github.com/bobmatnyc/trusty-tools/pull/2916)) ([`488602d`](https://github.com/bobmatnyc/trusty-tools/commit/488602dfa5cc75916f33c66b555832ce310b0025))

## [0.2.1] — 2026-07-09

### Changed

- Add crates.io package metadata (keywords/categories/homepage/readme).

## [Unreleased]

### Changed

- hoist compress::tool_output from trusty-agents ([#1959](https://github.com/bobmatnyc/trusty-tools/pull/1959)) ([#1968](https://github.com/bobmatnyc/trusty-tools/pull/1968)) ([`7cf93b9`](https://github.com/bobmatnyc/trusty-tools/commit/7cf93b9ab3918aff316238bdfe540a4053aa971d))
- publish trusty-agents-common 0.1.3 + trusty-mpm 0.11.0 to crates.io ([#1750](https://github.com/bobmatnyc/trusty-tools/pull/1750)) ([`70194ec`](https://github.com/bobmatnyc/trusty-tools/commit/70194ec1788fed2e71016912dae4e062baade139))
