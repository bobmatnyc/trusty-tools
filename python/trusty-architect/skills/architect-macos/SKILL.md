---
name: architect-macos
description: Framework skill. Load before any launchctl bootout, bootstrap or kickstart, when a Claude Code update lands, when a readdir or stat hangs in U-state on /Volumes, or before any step that could touch a macOS system daemon. Covers the launchd domain, restart from a Background session, the Full Disk Access procedure, and the fseventsd precedent.
---

# Architect macOS operations (framework)

Kind: framework. Index line in architect.md: load this before any launchctl command, on a Claude Code update, or on a U-state hang.

## Launchd domain

<!-- rule:r-129 -->
**Launchd domain is `gui/$(id -u)`.** Every trusty LaunchAgent lives in the user's GUI domain. `launchctl bootout gui/<other uid>/...` returns 0 as a no-op, and `bootstrap` into the wrong uid fails with 125 (or 5). Use `gui/$(id -u)`. A sandboxed agent shell may report managername "Background"; the Architect runs the bootstrap with the sandbox disabled when the user rules it.

<!-- rule:r-130 -->
**Restart a trusty LaunchAgent from a Background session (2026-10-06 00:59Z).** tmux-hosted shells report `launchctl managername` = Background, so `bootstrap` may be refused and a prior bootout would leave the service down. When the plist is unchanged and the binary on disk is the new one, use one `launchctl kickstart -k gui/$(id -u)/<label>` instead (worked for com.trusty.search 00:59:19Z: pid 80749 -> 81308, v0.56.0). Use bootout/bootstrap only from an Aqua session (the user's Terminal) or when the plist changed. 2026-10-06 07:56Z: bootout + bootstrap of com.trusty.memory from a Background-managername tmux shell on the iTerm2-started server, with the sandbox disabled, worked (rc 0 both; pid 34058 -> 52590, new env loaded).

## Claude Code update and Full Disk Access

<!-- rule:r-131 -->
**Claude Code update → Full Disk Access (owner, 2026-10-05 12:42Z).** The disk access issue was likely due to a new version of Claude Code needing FDA. Note that when cc is updated. The 8a SSD1/SSD2 readdir hang (2026-10-05 ~09:00-12:24Z) is attributed (owner, likely) to a new Claude Code binary that had no macOS Full Disk Access yet. Each version is a new binary under `~/.local/share/claude/versions/<ver>`, so TCC can treat it as a new app. On every Claude Code update (a new version in that dir, or the status line "Update installed · Restart to update" followed by restarts):
1. Tell the owner in the same pass: grant FDA to the new binary (System Settings > Privacy & Security > Full Disk Access). This is a UI-only action for the owner.
2. Until the owner confirms, treat new U-state readdir/stat hangs on /Volumes/* as FDA first, not as drive failure. Do not kill the hung processes; record them.
3. Record the version and the owner's confirmation in `local/state.md`.

The per-version grant log is in the instance host skill (`instance-host`). The + picker greys out the version binaries: drag the file from Finder (Cmd+Shift+G ~/.local/share/claude/versions) into the list. iTerm/tmux FDA does NOT cover claude (probe denied before the per-version grant).

## Never touch system daemons

<!-- rule:r-132 -->
**Never kill fseventsd (owner, 2026-10-07 14:1xZ: "killing fseventd was a bad idea").** `sudo killall fseventsd` at ~14:06Z (ruling 8e, Architect-recommended from precedent 311) took down the tmux server and every Claude session: the server was recreated at 14:07:00Z with all 21 PM panes at a bare shell, the poller gone, and in-flight agents (builds, merges, live checks) lost. It also broke the owner's iTerm install. Precedent 311 is superseded. For memory pressure from fseventsd, report it and hold builders; never recommend killing it or any other system daemon.

Trigger note for recall (ruling 92): this precedent is a failure. Before recommending any system daemon action, run `memory_recall` for it and cite what came back.
