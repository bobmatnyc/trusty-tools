# trusty-code-gui

Desktop GUI shell for `trusty-code` (tcode), built with Tauri.

**License**: MIT

**Note**: This crate has `publish = false` and is not published to crates.io.

## Status

This is a **minimal scaffold** (issue #2983, `docs/specs/trusty-code-harness-ui.md`
DOC-39): a compiling, runnable shell with one placeholder view. It is not the
full DOC-39 UI — later slices add the session/activity screens as the tcode
REST gateway (#2983) lands.

## Architecture — thin client (DOC-39 §2.1)

The `tcode serve` daemon owns all logic. It serves JSON-RPC on its Unix
socket only and binds no TCP port (#6637). The GUI does no client-side
computation and no Tauri-native filesystem/privileged access:

- **Bridge**: a webview cannot dial a Unix socket, so this app's Rust side runs
  a bridge (`src/bridge/`) that dials the daemon's socket. The bridge opens no
  port of its own (#10166): the webview reaches its router in-process, through
  the `tcode-bridge://` custom URI scheme for REST calls
  (`src/bridge/scheme.rs`) and a Tauri IPC `Channel` for the two event streams
  (`src/bridge/channel.rs`). A credential minted per launch still guards every
  route (`src/bridge/serve.rs`).
- **Data fetching**: the Svelte frontend calls `fetch()` against
  `tcode-bridge://localhost`, and opens event streams with the
  `open_daemon_stream` command. It runs only inside Tauri's webview; there is no
  plain-browser mode.
- **Tauri IPC**: `get_daemon_url` and `get_daemon_token` hand the webview the
  bridge's URL and credential; `open_daemon_stream` and `close_daemon_stream`
  drive the event streams.
- **Platform**: macOS only. WebKit on macOS addresses the scheme as
  `tcode-bridge://localhost`; Windows and Android would use
  `http://tcode-bridge.localhost`, which this app does not support.

### Tech Stack

- **Backend**: Rust, Tauri 2
- **Frontend**: Svelte 5 (runes) + Vite + Tailwind CSS

## Daemon connection

The bridge dials the daemon's socket, `trusty-code.sock` in the daemon data
directory (`trusty_common::daemon_addr::daemon_socket_path("trusty-code")`).
`TRUSTY_DATA_DIR_OVERRIDE` moves that directory. Nothing listens on a TCP port,
so there is no URL to point a browser tab at; `pnpm dev` serves the UI for the
Tauri dev window only.

## Building

```bash
# Rust-only compile check (no frontend toolchain required)
cargo check -p trusty-code-gui

# Full dev loop (requires pnpm)
cd crates/trusty-code-gui/ui && pnpm install && pnpm dev
```

### Release Build

```bash
cd crates/trusty-code-gui
cargo tauri build
```

**macOS signing (the #2951 pattern; restored here after being
intentionally omitted at scaffold time):** `tauri.conf.json` pins
`bundle.macOS.signingIdentity` to Bob's Developer ID
cert (`Developer ID Application: Bob Matsuoka (4JH68XUHC5)`) — the same
identity the retired `trusty-mpm-gui` used — so the `.app` bundle gets a stable TCC
identity instead of a fresh ad-hoc one per rebuild. On a machine without that
exact certificate in the login keychain, `cargo tauri build` will fail to
sign — override it with the `APPLE_SIGNING_IDENTITY` environment variable,
which Tauri's bundler honors in place of the config value:

```bash
# Local ad-hoc build (no Developer ID cert required):
APPLE_SIGNING_IDENTITY=- cargo tauri build

# Or sign with a different Developer ID cert you do have:
APPLE_SIGNING_IDENTITY="Developer ID Application: Your Name (TEAMID)" cargo tauri build
```

`trusty-code-gui` is excluded from the workspace's `default-members` and from
CI (`--exclude trusty-code-gui`): it needs the
pnpm/Svelte UI toolchain (and, for `cargo tauri build`, a platform WebView
runtime) that CI's headless runner does not provide, so a bare
`cargo build`/`check` (no `--workspace`, no `-p`) skips it automatically;
`--workspace` and `-p trusty-code-gui` still work as documented.
