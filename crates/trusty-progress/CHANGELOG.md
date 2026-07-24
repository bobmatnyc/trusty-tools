# Changelog — trusty-progress

All notable changes to trusty-progress are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

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
  without corrupting the redraw region.

## [0.1.1] — 2026-07-09

### Changed

- Add crates.io package metadata (keywords/categories/homepage/readme).
