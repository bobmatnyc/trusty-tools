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

When you ran `tctl install trusty-mpm`, three members were installed —
normally to `~/.local/bin` (the prebuilt path), or `~/.cargo/bin` if the
installer fell back to `cargo install` for your platform:

- **`trusty-mpm`** (binaries: `trusty-mpm` and `tm`) — the session orchestrator. Manages Claude Code sessions, coordinates with daemons, provides the CLI and daemon interface.
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
TRUSTY_VERSION=x.y.z curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh
```

**Environment variables:**
| Variable | Effect |
|---|---|
| `TRUSTY_VERSION` | Pin a release version; default: latest |
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
# On macOS — each daemon's own `service install` is the restart. It writes the
# unit, evicts any agent an older installer registered under a different label,
# reloads only when something actually changed, and puts the previous unit back
# if the new one fails to load ([#4868]).
trusty-search service install
trusty-memory service install

# trusty-mpm's daemon is supervised rather than self-installing, so it restarts
# via kickstart:
launchctl kickstart -k gui/$(id -u)/com.trusty.mpm
```

Do not hand-run a `launchctl bootout` / `bootstrap` pair against a plist path.
That is what this page used to advise, and it named
`com.trusty.trusty-search.plist` — a file that no longer exists, because
`service install` deletes it as a legacy alias of `com.trusty.search`
([#4868]). A bootout/bootstrap pair also cannot evict a unit registered under a
different label, and leaves the daemon down if the bootstrap fails.

[#4868]: https://github.com/bobmatnyc/trusty-tools/issues/4868

On Linux, use `systemctl`:
```bash
systemctl --user stop trusty-search trusty-memory trusty-mpm
systemctl --user start trusty-search trusty-memory trusty-mpm
```

### Q: How do I uninstall?

**On macOS, stop the daemons first, while their binaries still exist to do
it.** Unloading before deleting matters both ways: a daemon left running after
its binary is gone gets no cleaner, and a plist deleted while its job is still
registered leaves launchd running a copy you can no longer address by file.

```bash
trusty-search service uninstall 2>/dev/null || true
launchctl bootout gui/$(id -u)/com.trusty.memory 2>/dev/null || true
launchctl bootout gui/$(id -u)/com.trusty.mpm 2>/dev/null || true
rm -f ~/Library/LaunchAgents/com.trusty.*.plist
```

**Then remove the binaries.** They can land in either of two directories,
depending on how they were installed — a prebuilt `tctl install` (or the
one-liner) uses `~/.local/bin` (overridable via `TRUSTY_INSTALL_DIR`); a
`cargo install` or build-from-source path uses `~/.cargo/bin` (or
`$CARGO_HOME/bin` if you've overridden `CARGO_HOME`). Check both — `rm -f` so
a name missing from one directory doesn't stop the command:

```bash
rm -f ~/.cargo/bin/{trusty-mpm,tm,trusty-memory,trusty-search,tctl} \
      ~/.local/bin/{trusty-mpm,tm,trusty-memory,trusty-search,tctl}
```

(`trusty-mpm` installs two binaries, `trusty-mpm` and `tm` — both need
removing. Installed an optional member too, e.g. `trusty-analyze` or
`trusty-console`? Add its name to both brace lists.)

**If you installed via mise** (`mise use -g cargo:trusty-mpm`, or mise's
automatic reshimming of `~/.cargo/bin` after a plain `cargo install`), the
command on your `$PATH` may be a shim in `~/.local/share/mise/shims/`, not the
binary itself — check with `which -a tm` (or any of the other names above). The
`rm -f` above does not touch that shim file, so run `mise reshim` afterward to
prune it:

```bash
mise reshim
```

Skip that step and the shim lingers on disk; invoking it fails loudly rather
than silently, e.g.:

```
mise ERROR trusty-mpm is not a valid shim. This likely means you uninstalled a
tool and the shim does not point to anything. Run `mise use <TOOL>` to
reinstall the tool.
```

Confirm everything is gone:

```bash
command -v trusty-mpm tm trusty-memory trusty-search tctl
```

Every name should print nothing.

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

4. **Fall back to cargo install:** If prebuilts fail, the installer falls back to `cargo install`. Ensure Rust 1.94+ is installed:
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
| `tctl stack doctor [member]` | Deep diagnostic sweep — tools×scope matrix, drill-down, remediation. |
| `tctl doctor --self-check <member>` | Per-member DOC-1 conformance self-check (contract envelope, `verbs[]`, secret redaction). |
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
