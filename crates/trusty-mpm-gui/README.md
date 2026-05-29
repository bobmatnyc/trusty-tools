# trusty-mpm-gui

MPM desktop GUI application built with Tauri. Provides a graphical interface to trusty-mpm daemon functionality (agent orchestration, memory management, session control, Telegram bot administration).

**License**: Elastic License 2.0

**Note**: This crate has `publish = false` and is not published to crates.io.

## Purpose

`trusty-mpm-gui` provides desktop UI for:
- View and manage MPM daemon status
- Monitor agent execution and dispatch
- Browse and search memory palace
- View session history and logs
- Administer Telegram bot interactions
- Configure agent settings and access control

## Architecture

### Tauri Proxy

The GUI runs as a Tauri app and communicates with the trusty-mpm daemon via:
- **HTTP API**: Standard REST endpoints on daemon's HTTP server
- **WebSocket**: Real-time updates (session logs, agent status)
- **IPC**: Native Tauri command callbacks for system integration

### Tech Stack

- **Frontend**: Svelte (SvelteKit)
- **Build system**: Tauri (Rust + Webview)
- **Communication**: HTTP, WebSocket, Tauri commands
- **Styling**: Tailwind CSS
- **Bundling**: dmg (macOS), AppImage (Linux), .msi (Windows)

## Building

### Prerequisites

```bash
# Install Node/pnpm
npm install -g pnpm

# Install Tauri CLI
cargo install tauri-cli

# Install Rust dependencies
cargo build -p trusty-mpm-gui
```

### Development Build

```bash
cd crates/trusty-mpm-gui
pnpm install
cargo tauri dev
```

This starts the Tauri dev server with hot reload.

### Release Build

```bash
cd crates/trusty-mpm-gui
cargo tauri build
```

Builds optimized binaries for the current platform:
- macOS: `.app` bundle
- Linux: AppImage or deb package
- Windows: `.msi` installer

## Configuration

### Daemon Connection

The GUI discovers the trusty-mpm daemon via:

1. Default location: `http://localhost:7687`
2. Environment variable: `TRUSTY_MPM_DAEMON_URL`
3. User-specified in app settings

```json
{
  "daemon": {
    "url": "http://localhost:7687",
    "reconnect_interval_secs": 5,
    "timeout_secs": 30
  }
}
```

### App Settings

Stored in platform-standard locations:
- macOS: `~/Library/Application Support/trusty-mpm-gui/settings.json`
- Linux: `~/.config/trusty-mpm-gui/settings.json`
- Windows: `%APPDATA%\trusty-mpm-gui\settings.json`

```json
{
  "theme": "dark",
  "auto_connect": true,
  "log_level": "info",
  "window": {
    "width": 1400,
    "height": 900
  }
}
```

## User Interface

### Dashboard

Overview of daemon status:
- Agent execution counts (running, queued, completed)
- Recent sessions with status
- Quick links to common tasks
- Daemon health and uptime

### Session Management

Browse and manage MPM sessions:
- List all sessions (active, completed, failed)
- View session details (user, start time, duration)
- Stop running sessions
- View session logs and stderr output

### Memory Palace Browser

Search and view organizational memory:
- Query memory by keyword
- Filter by type (person, place, event, concept, preference, fact)
- View related memories (edges, connections)
- Edit memory nodes (with permission checks)
- Export memories to JSON

### Agent Monitor

Real-time monitoring of agent execution:
- Active agents and their dispatch status
- Pending tasks queue
- Agent performance metrics
- Failure logs and retry attempts

### Settings

Configuration and administration:
- Daemon URL and connection settings
- Agent whitelist and permissions
- Telegram bot configuration
- Access control policies
- Audit log browsing

## Features

### Real-Time Updates

WebSocket connection provides live updates:
- Agent status changes
- Session completion notifications
- Memory modifications
- Daemon health events

### Search

Full-text search across:
- Session logs
- Memory palace
- Agent output
- Audit events

### Export

Export functionality:
- Session transcript to text/JSON
- Memory exports with relationships
- Audit logs for compliance

### Mobile Responsive

UI adapts to smaller screens (tablets, phones):
- Responsive navigation
- Touch-friendly controls
- Mobile-optimized tables

## Troubleshooting

### Daemon Connection Failed

```
Error: Cannot connect to daemon at http://localhost:7687

Troubleshooting:
1. Verify daemon is running: ps aux | grep trusty-mpmd
2. Check daemon URL setting
3. Check firewall/network access
4. View daemon logs: RUST_LOG=info trusty-mpmd
```

### UI Not Responding

```
Tauri app freezes or hangs

Solutions:
1. Check daemon logs for errors
2. Force quit and restart the app
3. Clear app cache: rm -rf ~/Library/Caches/trusty-mpm-gui (macOS)
4. Check available disk space
```

### Memory Search Slow

```
Memory palace searches taking >10s

Optimization:
1. Reduce number of results per query
2. Use more specific keywords
3. Filter by memory type
4. Check daemon memory usage (may be paging)
```

## Development

### Project Structure

```
crates/trusty-mpm-gui/
├── src-tauri/              # Rust/Tauri backend
│   ├── main.rs
│   ├── handlers/           # HTTP request handlers
│   └── config.rs
├── src/                    # Svelte frontend
│   ├── App.svelte
│   ├── routes/
│   ├── components/
│   └── lib/
├── tailwind.config.js
└── package.json
```

### Adding a New Page

1. Create Svelte component in `src/routes/+page.svelte`
2. Add navigation link in `src/App.svelte`
3. Implement HTTP API calls using `fetch()`
4. Add state management in `src/lib/store.ts`

### Testing

```bash
# Unit tests (Rust)
cargo test -p trusty-mpm-gui

# Frontend tests (if configured)
pnpm test

# Integration test
cargo tauri dev  # manual testing
```

## Performance

- **Bundle size**: ~80-120 MB (platform-dependent, includes Chromium/Webkit)
- **Memory**: ~200-400 MB at runtime
- **Startup time**: ~2-3 seconds
- **Search responsiveness**: <500ms for typical queries

## Security

- **Daemon authentication**: API key or mTLS (future)
- **Memory access control**: Respects node visibility (private/company/public)
- **No credential storage**: OAuth tokens handled by daemon
- **Code signing**: Binaries signed with Apple/Windows certificates (official releases)

## Platform Support

- **macOS**: 10.15+
- **Linux**: glibc 2.31+ (Ubuntu 20.04+, Fedora 32+)
- **Windows**: 10+

## See Also

- `crates/trusty-mpm/README.md` for daemon API reference
- `crates/trusty-memory/README.md` for memory palace
- `docs/trusty-mpm/` for architecture and design
