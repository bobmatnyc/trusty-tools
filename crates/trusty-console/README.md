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
# Start the console (default port 7788)
trusty-console serve

# Custom port + open browser
trusty-console serve --http 127.0.0.1:9000 --open
```

## Building the UI

The Svelte SPA is built automatically by `build.rs` when `pnpm` is on PATH.
To skip the UI build (CI, no Node):

```bash
SKIP_UI_BUILD=1 cargo build -p trusty-console
```

A vanilla-JS placeholder (`ui/dist/index.html`) is committed to the repo so the
binary always embeds a working UI even without a Node build step.
