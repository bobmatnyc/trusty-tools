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

- Phase-1 status bar: readiness + budget chrome per DOC-39 §6.2 (refs #2983).
  `StatusBar.svelte` polls `GET /sessions` then `GET /sessions/{id}/readiness`
  (REST Slice 2, squash 15156b42) every 5s via a Svelte 5 `$effect` whose
  teardown clears the interval and aborts a shared `AbortController` (every
  poll's `fetch()` calls carry its signal, and `refresh()` re-checks
  `signal.aborted` after each `await`), so an in-flight poll is genuinely
  cancelled on unmount rather than merely ignored; renders a `daemon-unreachable` /
  `no-session` / `ready` state — same thin-client, no-Rust-proxying pattern as
  the existing `HealthPanel`. `pickActiveSession` (`lib/session-status.ts`)
  is the one piece of client-side logic: with no session picker yet (Phase
  2+ per DOC-39 §6.3) it picks the most recently created non-terminal
  session to reflect. Mounted as `<StatusBar>` in `App.svelte`, structurally
  a **sibling of `.body`**, never nested inside it, per DOC-39 §8.1 /
  AC-18.1 — pinned by a new `App.test.ts` DOM-structure test (`pnpm test`,
  vitest + jsdom, new devDependencies). **Data gap:** the budget half of the
  status bar renders a labeled "unavailable" placeholder — `Event::ContextBudget`
  is emitted on the SSE stream but never cached on the session the way
  `IndexReadinessSnapshot` is, so there is no `session.get_context_budget`
  RPC or `GET /sessions/{id}/budget` REST route to poll yet; tracked as a
  REST-slice follow-up rather than adding a new daemon endpoint in this PR.
- Initial scaffold: Tauri 2 + Svelte 5 desktop shell for the `trusty-code`
  (tcode) daemon, mirroring `crates/trusty-mpm-gui`'s structure (refs #2983,
  `docs/specs/trusty-code-harness-ui.md`). A single `get_daemon_url` IPC
  command exposes the configured daemon base URL (`TRUSTY_CODE_URL`, default
  `http://127.0.0.1:7881`); all daemon data (starting with `GET /health`) is
  fetched directly from the frontend via `fetch()`, never proxied through
  Rust, per DOC-39 §2.1's thin-client rule. Minimal working shell only — the
  full DOC-39 screens land in later slices as the REST gateway (#2983) adds
  routes.
