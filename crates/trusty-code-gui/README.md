# trusty-code-gui

Desktop GUI shell for `trusty-code` (tcode), built with Tauri. Mirrors
`crates/trusty-mpm-gui`'s structure and stack so the two desktop shells share
one set of conventions.

**License**: MIT

**Note**: This crate has `publish = false` and is not published to crates.io.

## Status

This is a **minimal scaffold** (issue #2983, `docs/specs/trusty-code-harness-ui.md`
DOC-39): a compiling, runnable shell with one placeholder view. It is not the
full DOC-39 UI — later slices add the session/activity screens as the tcode
REST gateway (#2983) lands.

## Architecture — thin client (DOC-39 §2.1)

The `tcode serve --http` daemon owns all logic. The GUI does no client-side
computation and no Tauri-native filesystem/privileged access — web and Tauri
builds must be behaviorally identical:

- **Data fetching**: the Svelte frontend calls the daemon's HTTP API directly
  via `fetch()` (currently `GET /health`), whether it runs inside Tauri's
  webview or a plain browser tab.
- **Tauri IPC**: exactly one command, `get_daemon_url`, which echoes the
  configured daemon base URL. It exists only because a plain web page cannot
  read the `TRUSTY_CODE_URL` environment variable — the native process can.

### Tech Stack

- **Backend**: Rust, Tauri 2
- **Frontend**: Svelte 5 (runes) + Vite + Tailwind CSS

## Daemon connection

Default base URL: `http://127.0.0.1:7881`, matching
`trusty_code::serve::DEFAULT_HTTP_PORT` (`crates/trusty-code/src/serve/mod.rs`).
Override with the `TRUSTY_CODE_URL` environment variable (Tauri mode) or the
`trusty-code.daemonUrl` `localStorage` key (web mode).

## Building

```bash
# Rust-only compile check (no frontend toolchain required)
cargo check -p trusty-code-gui

# Full dev loop (requires pnpm)
cd crates/trusty-code-gui/ui && pnpm install && pnpm dev
```

`trusty-code-gui` is excluded from the workspace's `default-members` and from
CI (`--exclude trusty-code-gui`), matching `trusty-mpm-gui`: it needs the
pnpm/Svelte UI toolchain (and, for `cargo tauri build`, a platform WebView
runtime) that CI's headless runner does not provide, so a bare
`cargo build`/`check` (no `--workspace`, no `-p`) skips it automatically;
`--workspace` and `-p trusty-code-gui` still work as documented.
