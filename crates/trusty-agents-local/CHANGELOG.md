# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---
## [Unreleased]

### Changed

- **BREAKING (behavioral):** `trusty-agents-local` no longer depends on `cto-assistant` and no longer calls `trusty_agents::install_plugins(...)` at startup. The binary is now a thin pass-through to `trusty_agents::run()`. Running `trusty-agents-local` no longer exposes the CTO-assistant persona or its CTO DB tools. This severs the `trusty-agents-local -> cto-assistant` Cargo dependency edge as part of architecture-review tranche 0 (item 4), ahead of cto-assistant's planned migration directly into `trusty-agents`.
