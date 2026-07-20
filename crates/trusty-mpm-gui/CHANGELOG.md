# Changelog — trusty-mpm-gui

All notable changes to trusty-mpm-gui are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [Unreleased]

### Changed

- Migrated the Svelte UI from its placeholder indigo/slate palette to Foundry v2 design tokens (closes #3488, part of epic #3486): `ui/src/app.css` now defines `--color-*` RGB-triple custom properties (light `:root` + `[data-theme='dark']`) sourced 1:1 from `docs/design/UI/design-system/tokens.css`, and `ui/tailwind.config.js` resolves every `trusty-*`/`status-*` color through `rgb(var(--color-*) / <alpha-value>)` instead of hardcoded hex literals — the same CSS-var bridge `crates/trusty-code-gui/ui` uses, keeping the crates in lockstep for a future `scripts/check_token_drift.mjs` enforcement flip. Dark-mode activation switched from a bare `<html class="dark">` to the `[data-theme='dark']` attribute (`ui/src/stores/theme.ts`); the existing manual `ThemeToggle` + `localStorage`-persisted preference is unchanged, only the DOM attribute it drives. All components dropped their `-light` token suffix + `dark:` variant pairs in favor of the single self-theming token each now resolves through.

### Security

- Set an explicit Content-Security-Policy (`default-src 'self'; connect-src 'self' ipc: http://ipc.localhost http://127.0.0.1:7880; style-src 'self' 'unsafe-inline'`) in `tauri.conf.json`, replacing `"csp": null` (architecture-review tranche 0). Added a hand-authored `capabilities/default.json` (`core:default` only, scoped to the `main` window) so the app's ACL is explicit rather than implicit.

### Fixed

- Stopped the recurring macOS "'trusty-mpm' would like to access data from other apps" TCC prompt caused by the GUI (closes #2951): renamed `productName`/window title from the bare `"trusty-mpm"` to `"Trusty MPM Dashboard"` so any future prompt is visibly the GUI, not the CLI/daemon; changed the bundle identifier from `com.trusty-mpm.gui` to `com.trusty.trusty-mpm.gui` and wired `bundle.macOS.signingIdentity` in `tauri.conf.json` so `cargo tauri build` produces a Developer-ID-signed `.app` with a stable designated requirement instead of a fresh ad-hoc identity per rebuild.

### Documentation

- Documented the cert-less build escape hatch for `cargo tauri build` (#2957 review follow-up): `bundle.macOS.signingIdentity` in `tauri.conf.json` is hardcoded to Bob's Developer ID cert, so building the bundle on any other machine requires either that exact certificate in the keychain or an `APPLE_SIGNING_IDENTITY` environment variable override (`APPLE_SIGNING_IDENTITY=-` for a local ad-hoc build, or a different Developer ID string) — Tauri's bundler honors the env var in place of the config value. See the README's "Release Build" section and `docs/reference/common-pitfalls.md`.
