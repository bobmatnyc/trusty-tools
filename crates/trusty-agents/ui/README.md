# Trusty Agents desktop UI

A Tauri 2 + Svelte 4 chat interface for [trusty-agents](../) (`tagent`). Lets you:

1. Chat with **CTRL**, the top-level controller (runs in the repo cwd).
2. Register project paths and chat with **project-scoped PMs** that run with
   that directory as the working directory.
3. Browse recent **task history** (score, cost, status) in the sidebar.

## Stack

- Tauri 2 (Rust backend)
- Svelte 4 + TypeScript + Vite 5 + Tailwind CSS 3
- `@tauri-apps/api` v2 for `invoke` / `listen`
- `lucide-svelte` for icons

## How it works

- The Tauri shell spawns `tagent --api --port 7654` as a sidecar on startup.
- Frontend `invoke('send_message', {...})` calls the Rust handler, which posts
  to `POST /api/task`, polls `GET /api/task/:id`, and emits `task-progress` /
  `task-complete` / `task-error` Tauri events to stream into the chat bubble.
- Project paths from the sidebar are forwarded to the API as `project_path`,
  which `src/api/server.rs` uses as the spawned workflow subprocess's cwd.

## Prerequisites

- Rust 1.80+ (for Tauri)
- Node.js 20+ and `pnpm` (or npm / yarn)
- A `tagent` binary on `$PATH` OR a debug/release build sitting in
  `../target/{debug,release}/tagent`.

## Running

The UI is dual-mode: the same code runs as a Tauri desktop shell (full
process spawning + native events) or as a plain browser SPA backed by the
`tagent --api` HTTP server. A small pill in the top-right of the window
shows which mode is active (`⊞ Desktop` vs `⟳ Web`).

### Desktop mode (Tauri)

The Tauri shell auto-spawns the API sidecar — nothing else to start.

```sh
make ui-dev
# or, equivalently:
cd ui && pnpm tauri dev
```

### Web mode (browser)

Run the API server in one terminal, then Vite in another. Vite's dev
server proxies `/api/*` to the API port so the browser sees a same-origin
API and CORS is a non-issue:

```sh
# terminal 1: start the API
cargo run -- --api --port 7654

# terminal 2: start Vite (uses VITE_TAGENT_PORT to know where to proxy)
make ui-web
open http://localhost:5173
```

Override the API port if you ran the server with `--port` other than 7654:

```sh
cargo run -- --api --port 9000
cd ui && VITE_TAGENT_PORT=9000 pnpm dev
```

For talking to a remote/non-proxied API server (e.g. a deployed instance),
set `VITE_TAGENT_API` to the absolute URL — this skips the proxy and uses the
permissive CORS layer on the API server:

```sh
VITE_TAGENT_API=https://tagent.example.com pnpm build
```

## Testing

```sh
pnpm test:unit          # vitest — pure logic + mounted components (jsdom). Runs in CI.
pnpm run check          # svelte-check. Runs in CI.
pnpm test               # playwright e2e. NOT run in CI: needs a running server.
```

The Playwright specs need something serving the app at `TAGENT_URL`
(default `http://127.0.0.1:7654`, i.e. a real `tagent --api`). All `/api/*`
routes are stubbed inside the specs, so a plain Vite dev server works too and
is the fastest loop:

```sh
pnpm dev --host 127.0.0.1                                    # shell 1
TAGENT_URL=http://127.0.0.1:5173 pnpm test                   # shell 2
```

`tests/config-takeover.spec.ts` is the layout guard for the agent-config
takeover: it MEASURES the instructions editor's rendered height at two
viewports, because that sizing has regressed twice and jsdom (no layout
engine) can only check the classes that are supposed to produce it.

## Production build

```sh
cd ui
pnpm tauri build
```

Bundles land in `ui/src-tauri/target/release/bundle/`.

## Layout

```
ui/
├── package.json
├── vite.config.ts
├── svelte.config.js
├── tailwind.config.js
├── postcss.config.js
├── tsconfig.json
├── index.html
├── README.md
├── src/
│   ├── App.svelte          # root layout (sidebar + main)
│   ├── main.ts
│   ├── app.css             # tailwind directives
│   ├── components/
│   │   ├── Sidebar.svelte      # CTRL + projects + task history
│   │   ├── ChatView.svelte     # message bubbles, Tauri event listeners
│   │   ├── InputArea.svelte    # textarea + workflow picker + send
│   │   └── TaskHistory.svelte  # polls /api/tasks
│   ├── stores/
│   │   └── app.ts          # writable stores + helpers
│   └── lib/
│       └── transport.ts    # invoke() + listen() dual-mode
└── src-tauri/
    ├── Cargo.toml
    ├── build.rs
    ├── tauri.conf.json
    ├── capabilities/
    │   └── default.json
    ├── icons/
    └── src/
        └── main.rs         # 4 commands + sidecar spawn
```
