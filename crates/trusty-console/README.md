# trusty-console

Web dashboard that detects the trusty services running on your machine and
shows a home page with a card per service.

## Phase 0 (P0) scope

- Service detection: `trusty-search`, `trusty-memory`, `trusty-analyze`
- Three status levels: **Running** (daemon up + health-checked), **Available**
  (binary on PATH, no daemon), **Absent** (binary not installed)
- Embedded Svelte SPA served from the binary
- `GET /api/console/services` JSON API

## Usage

```bash
# Start the console (default: 127.0.0.1:7788 — localhost only)
trusty-console serve

# Custom port + open browser
trusty-console serve --http 127.0.0.1:9000 --open

# Expose on both localhost AND the machine's Tailscale IPv4 (tailnet + loopback)
trusty-console serve --tailscale

# Expose on all interfaces (LAN + tailnet); use only if you understand the exposure
trusty-console serve --http 0.0.0.0:7788
```

## Tailscale / tailnet exposure

`--tailscale` is the recommended way to make the console durable across tailnet
sessions without exposing it on the public LAN.

### How it works

When `--tailscale` is passed (or `TRUSTY_CONSOLE_BIND=tailscale` is set), the
console:

1. Runs `tailscale ip -4` to detect the machine's Tailscale IPv4 (e.g.
   `100.x.y.z`).
2. Binds **two** TCP listeners on the configured port:
   - `127.0.0.1:<port>` — loopback, for local tooling.
   - `<ts-ip>:<port>` — Tailscale IP, reachable from any tailnet client.
3. Serves the same router on both listeners.

Only the primary (loopback) address is written to the discovery file.

If `tailscale ip -4` fails (Tailscale not installed / not running), a warning
is logged and the server falls back to localhost-only — it does **not** crash.

### Tailnet URL

```
http://100.x.y.z:7788
```

Replace `100.x.y.z` with the machine's Tailscale IP (shown in `tailscale status`
or `tailscale ip -4`).

### Bind precedence (highest to lowest)

| Priority | Mechanism | Example |
|---|---|---|
| 1 | `--http <addr>` (non-default value) | `--http 0.0.0.0:9000` |
| 2 | `TRUSTY_CONSOLE_BIND` env var | `TRUSTY_CONSOLE_BIND=tailscale` |
| 3 | `--tailscale` flag | `trusty-console serve --tailscale` |
| 4 | Default | `127.0.0.1:7788` (local only) |

`TRUSTY_CONSOLE_BIND` accepts:
- `tailscale` — dual-listener mode (loopback + Tailscale IP).
- Any `<host>:<port>` string — explicit bind (same as `--http`).
- Empty / unset — no effect; falls through to next priority level.

### Durable tailnet exposure via launchd (macOS)

Add `TRUSTY_CONSOLE_BIND=tailscale` to the launchd plist so the env var is
present on every restart:

```xml
<key>EnvironmentVariables</key>
<dict>
    <key>TRUSTY_CONSOLE_BIND</key>
    <string>tailscale</string>
</dict>
```

Or pass the flag directly in `ProgramArguments`:

```xml
<key>ProgramArguments</key>
<array>
    <string>/Users/you/.cargo/bin/trusty-console</string>
    <string>serve</string>
    <string>--tailscale</string>
</array>
```

Either approach survives reboots without manual intervention.

### CORS

The server sets `CorsLayer::permissive()` (wildcard `*` on all origins). This
is intentional for a local developer dashboard and does not change with
`--tailscale`.

## Building the UI

The Svelte SPA is built automatically by `build.rs` when `pnpm` is on PATH.
To skip the UI build (CI, no Node):

```bash
SKIP_UI_BUILD=1 cargo build -p trusty-console
```

A vanilla-JS placeholder (`ui/dist/index.html`) is committed to the repo so the
binary always embeds a working UI even without a Node build step.
