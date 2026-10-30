---
name: architect-macos
description: Architect skill, macOS only. Load before any launchctl bootout, bootstrap or kickstart, when a Claude Code update lands, on a Full Disk Access question, when a readdir or stat hangs in U-state on /Volumes, or before any step that could touch a macOS system daemon. Covers the launchd domain, restarting a LaunchAgent from a Background session, the Full Disk Access procedure, and the never-kill-a-system-daemon rule. Architect only.
user-invocable: true
metadata:
  version: "0.1.0"
category: monitoring
tags: [architect, macos, launchd, daemons]
effort: medium
---

# architect-macos — macOS Operations

Load this before any launchctl command, on a Claude Code update, or on a
U-state hang.

## Launchd domain

<!-- rule:r-129 -->
**Launchd domain is `gui/$(id -u)`.** Every trusty LaunchAgent lives in the
user's GUI domain. `launchctl bootout gui/<other uid>/...` returns 0 as a
no-op, and `bootstrap` into the wrong uid fails with 125 (or 5). Use
`gui/$(id -u)`. A sandboxed agent shell may report managername "Background";
the Architect runs the bootstrap with the sandbox disabled when the user
rules it.

<!-- rule:r-130 -->
**Restart a trusty LaunchAgent from a Background session.** tmux-hosted
shells report `launchctl managername` = Background, so `bootstrap` may be
refused, and a prior bootout would leave the service down. When the plist is
unchanged and the binary on disk is the new one, use one
`launchctl kickstart -k gui/$(id -u)/<label>` instead, and confirm the new pid
and version afterwards. Use bootout and bootstrap only from an Aqua session
(the user's Terminal) or when the plist changed. A bootout and bootstrap from
a Background tmux shell can work with the sandbox disabled; check both return
codes and the new pid.

## Claude Code update and Full Disk Access

<!-- rule:r-131 -->
**Claude Code update: Full Disk Access.** Each Claude Code version is a new
binary under `~/.local/share/claude/versions/<ver>`, so TCC can treat it as a
new app with no Full Disk Access (FDA). A readdir or stat that hangs on an
external volume right after an update is FDA first, not drive failure. On
every Claude Code update (a new version in that dir, or the status line
"Update installed · Restart to update" followed by restarts):
1. Tell the user in the same pass: grant FDA to the new binary (System
   Settings > Privacy & Security > Full Disk Access). This is a UI-only
   action for the user.
2. Until the user confirms, treat new U-state readdir or stat hangs on
   /Volumes/* as FDA first, not as drive failure. Do not kill the hung
   processes; record them.
3. Record the version and the user's confirmation in `local/state.md`, and
   keep the per-version grant log in `local/instance.md`.

The + picker greys out the version binaries: drag the file from Finder
(Cmd+Shift+G ~/.local/share/claude/versions) into the list. FDA for the
terminal or tmux does not cover claude.

## Never touch system daemons

<!-- rule:r-132 -->
**Never kill fseventsd or any other system daemon.** Killing fseventsd can
take down the tmux server and every Claude session with it: every PM pane
drops to a bare shell, the poller dies, and in-flight agents (builds, merges,
live checks) are lost. For memory pressure from fseventsd or another system
daemon, report it and hold builders; never recommend killing it or any
other system daemon.

This rule records a failure. Before recommending any system daemon action,
run `memory_recall` on the `architect` palace for it and cite what came back.

## Related skills

- `architect-host-admin`: the memory-pressure and load measurements.
