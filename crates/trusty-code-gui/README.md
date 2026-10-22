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
computation and no Tauri-native filesystem/privileged access — web and Tauri
builds must be behaviorally identical:

- **Bridge**: a webview cannot dial a Unix socket, so this app's Rust side runs
  an HTTP+SSE bridge (`src/bridge/`). It dials the daemon's socket and serves
  the webview on an ephemeral loopback port, `127.0.0.1:0`, with a credential
  minted per launch (`src/bridge/serve.rs`). #10166 tracks replacing this
  remaining loopback listener with a non-TCP transport.
- **Data fetching**: the Svelte frontend calls the bridge via `fetch()` and
  `EventSource`, whether it runs inside Tauri's webview or a plain browser tab.
- **Tauri IPC**: `get_daemon_url` and `get_daemon_token` hand the webview the
  bridge's URL and credential. Only the native process knows them.

### Tech Stack

- **Backend**: Rust, Tauri 2
- **Frontend**: Svelte 5 (runes) + Vite + Tailwind CSS

## Daemon connection

The bridge dials the daemon's socket, `trusty-code.sock` in the daemon data
directory (`trusty_common::daemon_addr::daemon_socket_path("trusty-code")`).
`TRUSTY_DATA_DIR_OVERRIDE` moves that directory. There is no fixed port: the
bridge picks a free loopback port at launch and logs it. In web mode, set the
`trusty-code.daemonUrl` `localStorage` key to that bridge URL and
`trusty-code.daemonToken` to the bridge credential.

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
