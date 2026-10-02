# Install Checkpoint Runbook

Installation and safe restart of trusty-mpm binaries using `cargo install --git` with checkpoint validation and rollback procedures.

## Pre-install Pause Checkpoint

Before installing a new version, capture the current running state:

```bash
# Record the currently running pid and version
OLD_PID=$(pgrep -f "trusty-mpm" | head -1)
OLD_VERSION=$(cargo install --list | grep "trusty-mpm" | awk '{print $2}')

echo "Pre-install checkpoint:"
echo "  PID: $OLD_PID"
echo "  Version: $OLD_VERSION"
echo "  Timestamp: $(date -u +%Y-%m-%dT%H:%M:%SZ)"

# Store for rollback reference
export INSTALL_CHECKPOINT_PID="$OLD_PID"
export INSTALL_CHECKPOINT_VERSION="$OLD_VERSION"
```

If no trusty-mpm process is running, `$OLD_PID` will be empty — note this in your checkpoint.

## Install by git Revision

Always install from the canonical git repository using a specific revision. Never use `--path` (which loses provenance once the worktree is reclaimed), and never `cp` a binary (macOS code-signature SIGKILL on exec).

```bash
cargo install trusty-mpm \
  --git https://github.com/bobmatnyc/trusty-tools.git \
  --rev <commit-sha> \
  --locked \
  --force
```

Key constraints:

- **`--git`** specifies the canonical repository — never a local path or worktree.
- **`--rev`** pins the exact commit; branch names are not allowed.
- **`--locked`** uses the `Cargo.lock` from the git tree to ensure reproducible builds.
- **`--force`** overwrites a previously installed version.

Run this from **outside the workspace directory** so no local `[patch]` resolution applies. Typical location: your home directory or `/tmp`.

```bash
cd ~
cargo install trusty-mpm --git ... --rev ... --locked --force
```

Record the installation details:

```bash
INSTALL_SHA="<commit-sha>"
INSTALL_TIME="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
INSTALL_BIN="$(which trusty-mpm)"

echo "Install completed:"
echo "  SHA: $INSTALL_SHA"
echo "  Binary: $INSTALL_BIN"
echo "  Timestamp: $INSTALL_TIME"
```

## Launchd Restart with PID and Version Recording

After installation, restart the launchd job (if one is running) and record both the old and new process metadata.

### Stop the Running Job

```bash
launchctl bootout system/com.trusty-mpm.service
```

Wait for the process to exit fully:

```bash
until ! pgrep -f "trusty-mpm" > /dev/null; do
  sleep 0.5
done

echo "Old process exited. PID was: $INSTALL_CHECKPOINT_PID"
```

### Start the New Job

```bash
launchctl bootstrap system /Library/LaunchDaemons/com.trusty-mpm.plist
```

Wait for the process to start:

```bash
sleep 2
NEW_PID=$(pgrep -f "trusty-mpm" | head -1)
NEW_VERSION=$(trusty-mpm --version)

echo "New process started:"
echo "  PID: $NEW_PID"
echo "  Version: $NEW_VERSION"
echo "  Timestamp: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
```

### Record Transition Metadata

Write a local log entry or annotation:

```bash
cat > "$HOME/.trusty-mpm-install.log" << EOF
---
install_date: $(date -u +%Y-%m-%dT%H:%M:%SZ)
old_version: $INSTALL_CHECKPOINT_VERSION
old_pid: $INSTALL_CHECKPOINT_PID
new_version: $NEW_VERSION
new_pid: $NEW_PID
commit_sha: $INSTALL_SHA
binary: $INSTALL_BIN
---
EOF
```

## Per-Session tmux_server Pairing Check (Mandatory)

On each session start after restart, trusty-mpm must establish a pairing with the current `tmux_server` process. This is mandatory for CLI operation and agent coordination.

Trusty-mpm refuses to operate in a session that has already claimed a different `tmux_server` instance. If you are restarting the daemon after upgrading, you must also restart all open trusty-mpm/tm sessions:

```bash
# Verify the new trusty-mpm process is running
pgrep -f "trusty-mpm"

# Kill any existing session agents or CLI instances that hold the old pairing
pkill -f "tm session"  # Kills any running tm subcommands
pkill -f "trusty-mpm-agent"  # Kills any live agents from the old version

# Start a fresh session to establish the new pairing
tm session status
```

If you see a pairing conflict (typically an error about "tmux_server already bound to PID X"), your running `tmux_server` is still associated with the old trusty-mpm process. Kill it and restart:

```bash
pkill -f "tmux_server"
sleep 1

# Re-bootstrap the session with a fresh pairing
tm session status
```

## Post-install Checks

After restart and pairing validation, confirm the installation with these checks:

### 1. Version Match

```bash
trusty-mpm --version
cargo install --list | grep trusty-mpm
```

Both should report the same version. If they differ, the installation did not fully replace the old binary.

### 2. Binary Integrity

```bash
# Verify the binary is runnable
trusty-mpm --help

# Check code signature on macOS (should be ad-hoc or developer-signed)
codesign -d -v "$(which trusty-mpm)"
```

### 3. Daemon Status

```bash
# Verify launchd loaded the job
launchctl list | grep trusty-mpm

# Check daemon logs
log show --predicate 'process=="trusty-mpm"' --last 10m
```

### 4. Session Pairing

```bash
# Verify the session can connect to the new daemon
tm session status
tm session list
```

If any session reports a pairing mismatch or timeout, the daemon and CLI are not coordinating correctly. Review the "Per-Session tmux_server Pairing Check" section above and restart both.

## Rollback

If the new version is broken, return to the previous version:

```bash
# Stop the new version
launchctl bootout system/com.trusty-mpm.service

# Reinstall the old version
cargo install trusty-mpm \
  --git https://github.com/bobmatnyc/trusty-tools.git \
  --rev $INSTALL_CHECKPOINT_VERSION \
  --locked \
  --force
```

If you do not have the old commit SHA, check your install log:

```bash
cat "$HOME/.trusty-mpm-install.log"
```

After reinstalling, restart launchd and verify pairing:

```bash
launchctl bootstrap system /Library/LaunchDaemons/com.trusty-mpm.plist
sleep 2

# Kill sessions holding the new (broken) pairing
pkill -f "tmux_server"
pkill -f "tm session"

# Verify the old version is running
trusty-mpm --version
tm session status
```

## Troubleshooting

### Process won't start after restart

- Check the launchd status: `launchctl list com.trusty-mpm.service`
- Review daemon logs: `log show --predicate 'process=="trusty-mpm"' --level=debug`
- Verify the binary exists: `ls -la "$(which trusty-mpm)"`
- Verify code signature: `codesign -d -v "$(which trusty-mpm)"` (should not fail on macOS)

### Session pairing fails

- Ensure the daemon is running: `pgrep -f "trusty-mpm"`
- Kill all session clients: `pkill -f "tm session"`
- Restart the session: `tm session status`

### Binary is not updated

- Confirm `cargo install` ran with `--force`: `cargo install --list | grep trusty-mpm`
- Check that the binary path is correct: `which trusty-mpm`
- On macOS, verify the binary was not replaced by a `cp` (which breaks code signatures): use `cargo install`, never manual file copy.

### Need to return to a specific known-good version

Use the commit SHA from your pre-install checkpoint or install log to reinstall that version:

```bash
cargo install trusty-mpm \
  --git https://github.com/bobmatnyc/trusty-tools.git \
  --rev <old-commit-sha> \
  --locked \
  --force
```

Then follow the "Per-Session tmux_server Pairing Check" and "Post-install Checks" sections again.
