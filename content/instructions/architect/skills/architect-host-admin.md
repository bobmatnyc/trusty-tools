---
name: architect-host-admin
description: Architect skill, macOS host commands. Load for a resource or plan-usage report, a builder-band measurement, a host load or disk check, a subsystem-health check, a tmux priority question, a build-cache or disk question, or any step that touches a secret or backup. Holds the measurement commands the Architect cites. Architect only.
user-invocable: true
metadata:
  version: "0.1.0"
category: monitoring
tags: [architect, host, resources, subsystem-health, secrets]
effort: medium
---

# architect-host-admin — Host Administration

Load this for any resource report, band measurement, subsystem-health check,
secrets or backup step, or build-cache question. The commands are for a macOS
host.

## Resource reporting

<!-- rule:r-062 -->
**Resource reporting.** Report plan usage (5-hour session and weekly limits)
and host stress (load vs. core count, CPU idle, memory pressure, swap, the
tmux server's scheduling priority) in every report that reaches the user and
on any threshold crossing. Flag plan usage at 75%+ of a limit, load above
core count, memory free below 20%, or any swap in use. The measurement
commands are below.

<!-- rule:r-105 -->
**Plan limits.** Send `/usage` into an idle PM pane whose input box is `empty`
or `suggestion`, capture, then send `Escape`. It shows the 5-hour session and
weekly percentages with reset times; the figures are account-wide. The
trusty-mpm status line in each pane also shows the session and weekly
percentages.

After an account switch, the status line can keep the old account's figures
while `/usage` on the new account shows the real ones. Quote plan usage only
from `/usage` (the user's, or `/usage` in the Architect pane at an idle
boundary), and label status-line figures reported or stale after any account
change.

## Host metrics

<!-- rule:r-106 -->
**Host metrics:**
```sh
sysctl -n hw.ncpu
sysctl -n vm.loadavg  # L15 is the third field
df -g /System/Volumes/Data | tail -1  # Avail GiB, not /
memory_pressure | tail -1
sysctl -n vm.swapusage
```

Use the 15-minute load (the third loadavg field; the 5-minute load is too
twitchy) and the `Avail` GiB of the data volume, not `/` (the sealed system
volume always shows about 5% used). The band thresholds and their hysteresis
are set in your operating manual ("Builder slots follow CPU and disk"), with
the rule for who gets a band relay; apply them from there.

<!-- rule:r-108 -->
**Builder band command:**
```sh
sysctl -n hw.ncpu vm.loadavg; df -g /System/Volumes/Data | tail -1; memory_pressure | tail -1; sysctl -n vm.swapusage
```

## tmux priority

<!-- rule:r-109 -->
`ps -o pri= -p "$(tmux display-message -p '#{pid}')"`. `4` means background
QoS: every pane is limited to efficiency cores with throttled I/O. A tmux
server started by a launchd job with `ProcessType Background` gets this, and
it cannot be cleared at runtime. Only a new tmux server started from a
normal-priority context (an operator terminal) fixes it.

## Subsystem health

<!-- rule:r-064 -->
**Subsystem health.** Keep trusty-mpm and its subsystems, especially
trusty-memory and trusty-search, at peak efficiency. On every full-poll pass
(every 4th pass) and whenever a PM reports a tool failure, run the read-only
checks below, compare the results with the previous figures in
`local/state.md`, and act on a threshold crossing: non-destructive
maintenance the user has authorized (listed in `local/state.md`) runs
directly; anything else (deleting palaces, `--force` reindex of a populated
index, daemon restarts) goes to the user with evidence and a recommendation.
Framework defects follow the improvement pipeline (`architect-improvements`).

<!-- rule:r-110 -->
Read-only checks, one call each (MCP tools unless noted):

| Check | Tool / command | Watch for |
| --- | --- | --- |
| trusty-mpm fleet | `mcp__trusty-mpm__console_metrics`, `supervisor_status` | errored > 0, pending decisions, stale supervisor metrics, daemon version behind the installed `tm` |
| Recent errors (all daemons) | `mcp__trusty-mpm__list_recent_errors` | new fingerprints or rising counts; vector-lane loss (`usearch ... EMPTY in memory`), corpus open failures |
| trusty-search | `mcp__trusty-search__search_health`, `console_metrics` | `healthy: false`, `warm_boot_degraded`, watcher inactive, `vectors_unavailable_reason`, search `latency_ms` > 2000 |
| trusty-memory | `mcp__trusty-memory__console_metrics` | `ram_bytes` growth, drawers without vectors (drawer_count > vector_count), many empty palaces holding disk |
| Daemon memory | `ps -o rss= -p $(pgrep -f trusty-search)` (Bash) | RSS far above the index size on disk, or growth between checks |
| Disk | `mcp__trusty-mpm__disk_survey` | growth of palace store or index dirs |

Record each check's figures in `local/state.md` so the next check can
compare.

## Build caches

<!-- rule:r-112 -->
**Rust gate rules.**
1. A gate log counts only when it shows `Compiling` (clippy: `Checking`) for
   every changed crate in the dependency graph, from the gating worktree's
   path.
2. Never run `cargo clean` on a shared or exported target dir; it deletes
   other checkouts' output.
3. Never gate two worktrees of one crate at the same time.
4. A rustc or test binary at about 0 CPU for over 3 min is reported to the
   Architect by pid and never killed.
5. Stop a stalled agent before dispatching its replacement, so two agents
   never race on one task.

<!-- rule:r-113 -->
The shared build-cache pool is `~/.trusty-tools/cargo-target-pool/<org>/...`.
Size it with `du -sh ~/.trusty-tools/cargo-target-pool/*`; a `find` over the
home directory runs past 5 minutes.

<!-- rule:r-114 -->
Builder slots are seeded with `cp -c` (an APFS copy-on-write clone of the
shared target), so `du` over-counts slots that share blocks; judge cleanups
by `df` before and after.

## Secrets and backups

<!-- rule:r-115 -->
**Backing up secret-bearing files.** An Architect backup of a secret-bearing
file (a project env file, tfvars) goes under `~/backups/` (dir 700, file
600), never beside the original inside a repo, where a broad `git add` could
commit it; check `git check-ignore` if a backup must stay in place.

<!-- rule:r-116 -->
**Secrets into env stores.** `security find-generic-password -w` ends its
output with a newline. When piping it into `vercel env add` (or any store),
strip it: `... -w | tr -d '\r\n' | vercel env add NAME production --sensitive`.
A trailing newline makes the receiving service reject the key. Never echo the
value; check the length only.

## Related skills

- `architect-macos`: launchd and system-daemon rules.
- `architect-improvements`: where a framework defect goes.
