# Install and Run trusty-mpm (tm) on Your Laptop

## Quickstart (2 minutes)

```bash
# 1. One-liner: download and install tctl (the control plane)
curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh

# 2. Install trusty-mpm and its dependencies (memory + search daemons)
tctl install trusty-mpm

# 3. Verify installation
tctl status

# 4. Run tm in any git repo
cd ~/your-project
tm
```

**Done.** You now have trusty-mpm running and can start Claude Code sessions with `tm`.

---

## What You Just Installed

When you ran `tctl install trusty-mpm`, three binaries were installed to `~/.local/bin`:

- **`trusty-mpm`** (binary: `tm`) — the session orchestrator. Manages Claude Code sessions, coordinates with daemons, provides the CLI and daemon interface.
- **`trusty-memory`** — long-term memory storage daemon. Stores development context, notes, snippets. Started automatically as a macOS/Linux service.
- **`trusty-search`** — hybrid code search daemon (BM25 + vector + knowledge graph). Indexes your projects. Started automatically as a macOS/Linux service.

These three form the core of the trusty-mpm platform. The installer also sets up `tctl` (a transitional alias to `trusty-installer`, the control plane binary) which orchestrates installation, upgrades, and system health checks.

---

## Prerequisites

✓ **Claude Code** — You need Claude Code installed. If not, install it from https://claude.com/download.

✓ **Git & tmux** — Standard Unix tools. The installer can install these if missing; see the interactive prompts.

✓ **Rust (optional)** — Only needed if the prebuilt binary for your platform is unavailable. Installer falls back to `cargo install` automatically.

**Supported platforms:**
- macOS (Apple Silicon: M1, M2, M3, …)
- Linux (x86_64, arm64)

---

## Detailed Installation Guide

### Step 1: Download and Install the Installer

The one-liner (`curl | sh`) downloads a prebuilt `trusty-installer` binary, verifies its SHA-256 checksum, and installs it to `~/.local/bin`:

```bash
curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh
```

**What happens:**
1. Detects your platform (macOS arm64, Linux x86_64/arm64).
2. Downloads the latest prebuilt `trusty-installer` release.
3. Verifies the binary's SHA-256 checksum against the published value.
4. Installs `trusty-installer` and creates a `tctl` alias to `~/.local/bin`.
5. Offers to add `~/.local/bin` to your `$PATH` if it's not already there.
6. Runs `tctl install` (interactive) to begin the full stack install.

**Non-interactive mode** (for CI/scripts):
```bash
curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh -s -- -y
```

**Pin a specific version:**
```bash
TRUSTY_VERSION=0.3.0 curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh
```

**Environment variables:**
| Variable | Effect |
|---|---|
| `TRUSTY_VERSION` | Pin a version (e.g., `0.3.0`); default: latest |
| `TRUSTY_INSTALL_DIR` | Install directory; default: `~/.local/bin` |
| `TRUSTY_YES=1` | Skip all prompts (same as `-y` flag) |
| `TRUSTY_FORCE=1` | Re-download even if already installed |
| `TRUSTY_NO_MODIFY_PATH=1` | Don't modify shell PATH |

### Step 2: Install trusty-mpm and Dependencies

Once `tctl` is installed, install the full stack:

```bash
tctl install trusty-mpm
```

This command:
- Automatically installs trusty-mpm **and its transitive dependencies** (trusty-memory, trusty-search) in the correct order.
- Prefers prebuilt binaries (fast download + verify). Falls back to `cargo install` if prebuilts are unavailable.
- Health-gates each installation (verifies the binary exists and responds to `--version`).
- Starts daemons as system services (launchd on macOS, systemd on Linux).

**If you want all installed members (including trusty-analyze, trusty-console):**
```bash
tctl install
```

Or install a specific member:
```bash
tctl install trusty-search
```

### Step 3: Verify Installation

Check the status of all daemons:

```bash
tctl status
```

**Expected output:**
```
tctl status — stack summary
  trusty-search      0.31.0       up
  trusty-memory      0.18.2       up
  trusty-analyze     0.7.0        down
  trusty-review      0.6.4        down
  tga                2.8.0        n/a
  trusty-console     0.3.1        down
  trusty-mpm         0.16.0       up
verdict: healthy (exit 0)
```

The core members (`trusty-search`, `trusty-memory`, `trusty-mpm`) should be **up**. The analysis and review daemons are optional.

Run a deeper diagnostic:

```bash
tm doctor
```

This performs a full health check:
- Confirms all agents and skills are deployed.
- Verifies daemons are reachable (trusty-memory, trusty-search).
- Checks for orphaned session worktrees.
- Reports any misconfigurations.

**Expected output:**
```
trusty-mpm doctor
  ✅ instructions  instruction pipeline ran
  ✅ agents        55 agent(s) deployed
  ✅ skills        262 skill(s) available
  ✅ skill_source  19 skill file(s) available
  ✅ memory        trusty-memory healthy at 127.0.0.1:7070
  ✅ search        trusty-search healthy at 127.0.0.1:7878
  ✅ worktrees     no orphaned worktrees found
  ⚠️  gh_account    gh is not authenticated

overall: ⚠️ passed with warnings
```

⚠️ is OK; it's just warning that GitHub (`gh`) is not authenticated (optional for now).

### Step 4: First Run — Try tm

Navigate to any git repository and run:

```bash
cd ~/your-git-project
tm
```

On first run, you'll see the guided setup:
- Asks if you want to use Claude Code (you do).
- Configures the session name (defaults to the repo name).
- Launches a Claude Code session in a tmux window with trusty-mpm's framework loaded.

Inside the session, you have access to all trusty-mpm tools: `tm sessions`, `tm load`, `tm doctor`, memory recall, code search, and the full agent/skill ecosystem.

---

## Updating trusty-mpm

### Check for Updates

```bash
tctl updates
```

Shows available updates for all installed members, with changelog headlines.

### Upgrade

```bash
tctl upgrade
```

Upgrades all members to the BOM-pinned stable versions, then restarts daemons. Idempotent — safe to run multiple times.

**Upgrade a single member:**
```bash
tctl upgrade trusty-search
```

---

## Troubleshooting & Common Tasks

### Q: Do trusty-mpm and trusty-memory need macOS Full Disk Access?

**A: No.** Full Disk Access is required only for `trusty-search`, which may index external volumes (`/Volumes/…`).

`trusty-mpm` manages sessions and git worktrees under `$HOME` only — no TCC-protected paths. `trusty-memory` reads from `$HOME` only. Neither requires FDA re-granting.

If you see a warning about FDA, it applies to `trusty-search` only. Run `tm doctor` to see details.

### Q: How do I restart the daemons?

Use the graceful restart convention (SIGTERM, not SIGKILL):

```bash
# On macOS
launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/com.trusty-search.plist
launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/com.trusty-memory.plist
launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/com.trusty-mpm.plist

# Reinstall or run: cargo install --path … --locked

# Restart
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.trusty-search.plist
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.trusty-memory.plist
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.trusty-mpm.plist
```

On Linux, use `systemctl`:
```bash
systemctl --user stop trusty-search trusty-memory trusty-mpm
systemctl --user start trusty-search trusty-memory trusty-mpm
```

### Q: How do I uninstall?

The binaries live in `~/.local/bin`. Remove them:

```bash
rm ~/.local/bin/trusty-mpm ~/.local/bin/trusty-memory ~/.local/bin/trusty-search ~/.local/bin/tctl
```

On macOS, also remove the launchd plists:
```bash
rm ~/Library/LaunchAgents/com.trusty-*.plist
launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/com.trusty-search.plist 2>/dev/null || true
launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/com.trusty-memory.plist 2>/dev/null || true
launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/com.trusty-mpm.plist 2>/dev/null || true
```

### Q: Installation failed. What do I do?

1. **Check for missing tools:**
   ```bash
   which git tmux cargo
   ```
   If any are missing, the installer will prompt you to install them. Follow the guidance.

2. **Check network:** The installer downloads prebuilt binaries from GitHub Releases. Verify you can reach `github.com`:
   ```bash
   curl -I https://github.com/
   ```

3. **Check logs:** The installer writes logs to stderr. Save them and review:
   ```bash
   tctl install trusty-mpm 2>&1 | tee install.log
   ```

4. **Fall back to cargo install:** If prebuilts fail, the installer falls back to `cargo install`. Ensure Rust 1.91+ is installed:
   ```bash
   rustc --version
   cargo --version
   ```

5. **Run with verbose output:**
   ```bash
   tctl install trusty-mpm -v -v
   ```

### Q: How do I use trusty-mpm with an existing Claude Code project?

If you already have a Claude Code project that was NOT created with trusty-mpm, you can migrate it:

1. In your project directory, run:
   ```bash
   tm load
   ```

2. This provisions a trusty-mpm-managed workspace, sets up the configuration, and launches a new session.

3. All future sessions in that project will run under trusty-mpm's framework.

### Q: What is the difference between trusty-mpm and claude-mpm?

See [claude-mpm vs trusty-mpm — Differences & Install](./claude-mpm-vs-trusty-mpm.md) for a detailed comparison.

---

## Reference

### tctl Commands Cheat Sheet

| Command | What it does |
|---|---|
| `tctl install [members]` | Install trusty-mpm and dependencies (or named members). Default: all enabled. |
| `tctl upgrade [members]` | Upgrade to BOM-pinned versions. |
| `tctl updates` | List available updates + changelog. |
| `tctl status` | One-line stack summary. |
| `tctl doctor` | Full system health check. |
| `tctl config` | Print effective configuration. |
| `tctl version` | Print versions: installer + stack members. |
| `tctl start [members]` | Start daemon(s). |
| `tctl stop [members]` | Stop daemon(s). |
| `tctl restart [members]` | Gracefully restart daemon(s). |

### tm Commands Cheat Sheet

| Command | What it does |
|---|---|
| `tm` | Guided setup + launch a Claude Code session in a tmux window. |
| `tm doctor` | Full health check (agents, skills, daemons, worktrees). |
| `tm sessions` | List all active sessions. |
| `tm run <command>` | Run a command in a managed session. |
| `tm load` | Provision a managed workspace in the current project. |
| `tm --version` | Print trusty-mpm version. |
| `tm --help` | Show all available commands. |

### Environment Variables

| Variable | Effect |
|---|---|
| `TRUSTY_MEMORY_URL` | Override trusty-memory daemon URL (e.g., for CI). Default: auto-discovered. |
| `TRUSTY_SEARCH_URL` | Override trusty-search daemon URL. Default: auto-discovered. |
| `RUST_LOG` | Set tracing level (e.g., `RUST_LOG=debug` for verbose output). |

---

## Next Steps

- **Run your first session:** `cd ~/your-project && tm`
- **Understand the architecture:** Read [trusty-mpm architecture overview](../../crates/trusty-mpm/docs/ARCHITECTURE-MEMORY-SESSIONS-SEARCH.md)
- **Learn about sessions & worktrees:** See the architecture doc above.
- **Explore the broader ecosystem:** Read the [root README](../../README.md) for trusty-search, trusty-memory, and trusty-analyze.
