# Changelog — trusty-code-gui

All notable changes to trusty-code-gui are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [Unreleased]

### Fixed

- Restored the macOS Developer-ID signing config that was intentionally
  omitted at scaffold time, mirroring the `trusty-mpm-gui` pattern (#2951 /
  PR #2957): `tauri.conf.json` now pins `bundle.macOS.signingIdentity` to
  `Developer ID Application: Bob Matsuoka (4JH68XUHC5)` so `cargo tauri build`
  produces a stable-identity `.app` bundle instead of a fresh ad-hoc identity
  per rebuild. `productName`/window title (`"Trusty Code"`) and the bundle
  identifier (`com.trusty.trusty-code.gui`) were already stable from the
  original scaffold and are unchanged. Documented the cert-less
  `APPLE_SIGNING_IDENTITY=- cargo tauri build` escape hatch in the README and
  `docs/reference/common-pitfalls.md`. `trusty-code-gui` was already excluded
  from the workspace's `default-members` at scaffold time (#2983), so no
  change was needed there. `trusty-code`/`tcode` has no `SIGNABLE_BINARIES`
  entry or `tctl sign`-style fallback install script of its own yet, so no
  equivalent install-script wiring was added for `trusty-code-gui` (the Tauri
  config is the only signing path today).

### Added

- Initial scaffold: Tauri 2 + Svelte 5 desktop shell for the `trusty-code`
  (tcode) daemon, mirroring `crates/trusty-mpm-gui`'s structure (refs #2983,
  `docs/specs/trusty-code-harness-ui.md`). A single `get_daemon_url` IPC
  command exposes the configured daemon base URL (`TRUSTY_CODE_URL`, default
  `http://127.0.0.1:7881`); all daemon data (starting with `GET /health`) is
  fetched directly from the frontend via `fetch()`, never proxied through
  Rust, per DOC-39 §2.1's thin-client rule. Minimal working shell only — the
  full DOC-39 screens land in later slices as the REST gateway (#2983) adds
  routes.
