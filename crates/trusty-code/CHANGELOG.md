# Changelog — trusty-code

All notable changes to trusty-code are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [Unreleased]

### Added

- Surface trusty-search index readiness to daily-driver task sessions: after warming the project's index at task start, `ensure_project_indexed_in_background` now probes `GET /indexes/{id}/status` (via the shared `trusty_common::search_readiness::log_index_readiness`) and emits one stderr line describing which lanes are ready — `warn` while semantic embedding is still warming (expect lexical-only results), `info` once it is ready — so a session is never silently querying a not-yet-ready index ([#2784](https://github.com/bobmatnyc/trusty-tools/issues/2784))

## [0.1.0] — 2026-07-09

### Added

- Initial crates.io release.
