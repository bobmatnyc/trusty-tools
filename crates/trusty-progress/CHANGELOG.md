# Changelog — trusty-progress

All notable changes to trusty-progress are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [0.3.0] — 2026-08-10

### Changed

- **MSRV raised to Rust 1.94** (was 1.91). `aws-config` >= 1.9.0 and
  `aws-sdk-bedrockruntime` >= 1.136.0, published 2026-07-08, declare
  `rust-version = "1.94.1"`; because those are unpinned caret ranges in the
  workspace manifest, `cargo install` **without `--locked`** re-resolves into
  them and then refuses to build on rustc below 1.94.1 — the reported
  `cargo install trusty-code` failure on rustc 1.91.1. Users on rustc
  1.91-1.93 must `rustup update` before installing any `trusty-*` crate. See
  [ADR-0029](../../docs/adr/0029-msrv-1-94-and-edition-policy.md)
  ([#4928](https://github.com/bobmatnyc/trusty-tools/pull/4928))
- Version goes 0.2.0 → **0.3.0**, not 0.2.1. `trusty-progress` opts into the
  workspace floor with `rust-version.workspace = true`, so the published crate
  now advertises `rust-version = "1.94"`. A consumer on rustc 1.91-1.93 whose
  `^0.2` range resolved into a 0.2.1 patch would stop building; a minor step
  leaves those consumers pinned to 0.2.0 instead
  ([#4928](https://github.com/bobmatnyc/trusty-tools/pull/4928))

## [0.2.0] — 2026-07-23

### Added

- `LiveChecklist` / `ComponentState`: an in-place, per-component progress
  checklist built on `indicatif::MultiProgress` — one row per component,
  transitioning `pending -> downloading -> verifying -> installed / skipped /
  failed` without scrolling. Degrades automatically to a fully hidden,
  zero-byte no-op in any non-interactive `Output` mode (`Plain`/`Silent`),
  matching the existing `ProgressHandle` policy, so callers get a clean live
  demo view on a real terminal with no risk to piped/CI/`--json` output.
  `LiveChecklist::note` prints a line above the active rows
  (`MultiProgress::println`) for the rare message that must interleave
  without corrupting the redraw region. `enable_steady_tick` (a per-row
  ticker thread) is skipped entirely outside `Mode::Interactive` (code-critic
  review) so a piped/`--json`/CI invocation spawns zero ticker threads, not
  just zero visible bytes.

## [0.1.1] — 2026-07-09

### Changed

- Add crates.io package metadata (keywords/categories/homepage/readme).
