# open-mpm desktop UI

A Tauri 2 + Svelte 4 chat interface for [open-mpm](../). Lets you:

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

- The Tauri shell spawns `open-mpm --api --port 7654` as a sidecar on startup.
- Frontend `invoke('send_message', {...})` calls the Rust handler, which posts
  to `POST /api/task`, polls `GET /api/task/:id`, and emits `task-progress` /
  `task-complete` / `task-error` Tauri events to stream into the chat bubble.
- Project paths from the sidebar are forwarded to the API as `project_path`,
  which `src/api/server.rs` uses as the spawned workflow subprocess's cwd.

## Prerequisites

- Rust 1.80+ (for Tauri)
- Node.js 20+ and `pnpm` (or npm / yarn)
- An `open-mpm` binary on `$PATH` OR a debug/release build sitting in
  `../target/{debug,release}/open-mpm`.

## Running

The UI is dual-mode: the same code runs as a Tauri desktop shell (full
process spawning + native events) or as a plain browser SPA backed by the
`open-mpm --api` HTTP server. A small pill in the top-right of the window
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

# terminal 2: start Vite (uses VITE_OMPM_PORT to know where to proxy)
make ui-web
open http://localhost:5173
```

Override the API port if you ran the server with `--port` other than 7654:

```sh
cargo run -- --api --port 9000
cd ui && VITE_OMPM_PORT=9000 pnpm dev
```

For talking to a remote/non-proxied API server (e.g. a deployed instance),
set `VITE_OMPM_API` to the absolute URL — this skips the proxy and uses the
permissive CORS layer on the API server:

```sh
VITE_OMPM_API=https://ompm.example.com pnpm build
```

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
