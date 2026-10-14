---
name: architect-host-admin
description: Framework skill. Load for a resource or plan-usage report, a builder-band measurement, a host load or disk check, a subsystem-health check, a tmux priority question, a build-cache or disk question, or any step that touches a secret or backup. Holds the measurement commands and thresholds the Architect cites.
---

# Architect host administration (framework)

Kind: framework. Index line in architect.md: load this for any resource report, band measurement, subsystem-health check, secrets or backup step, or build-cache question.

## Resource reporting

<!-- rule:r-062 -->
- **Resource reporting.** Report plan usage (5-hour session and weekly limits) and host stress (load vs. core count, CPU idle, memory pressure, swap, the tmux server's scheduling priority) in every report that reaches the user and on any threshold crossing. Flag plan usage at 75%+ of a limit, load above core count, memory free below 20%, or any swap in use. See the measurement commands below.

<!-- rule:r-105 -->
**Plan limits:** Send `/usage` into an idle PM pane whose input box is `empty` or `suggestion`, capture, then send `Escape`. It shows the 5-hour session and weekly percentages with reset times; the figures are account-wide. The trusty-mpm status line in each pane also shows `⏳<session>% 📅<week>%`.

After an account switch, the status line's ⏳ (5h), 📅 (weekly) and 💸 fields kept the old account's figures (weekly 91-92%) while `/usage` on the new account showed 5%. Quote plan usage only from `/usage` (the owner's, or `/usage` in the Architect pane at an idle boundary), and label status-line figures reported/stale after any account change.

### Usage tracking

Every reading per account goes to `local/usage/ledger.jsonl` (source `statusline-cache`, `usage-cmd` or `manual`). `fleet-poll.py` samples each cycle and raises `usage` alerts: the 5h window projected to run out within 45 min, weekly at 80% and 90%, and a recommended cutover (names the next account and the switch time). The Architect's own `statusLine.command` is `scripts/statusline-tee.sh`, which caches Claude Code's `rate_limits` with the account; the poller falls back to `/usage` in one idle pane at most hourly, and at once after an account change. Poller changes need a restart of `the poller session`.

```sh
scripts/usage.py summary   # per-account table, burn rate, projection
scripts/usage.py plan      # cutover recommendation, weekly runway
scripts/usage.py sample    # one reading from the statusline cache
echo "<capture>" | scripts/usage.py ingest --source manual   # a /usage capture
```

Record a manual reading (the owner's usage page) as a ledger line with `"source": "manual"`; a 5h window that has not started is `pct5h` 0 with `reset5h` null.

### Host metrics

<!-- rule:r-106 -->
**Host metrics:**
```sh
sysctl -n hw.ncpu
vm.loadavg  # for L15 (third field)
df -g /System/Volumes/Data | tail -1  # Avail GiB, not /
memory_pressure | tail -1
sysctl -n vm.swapusage
```

Use the 15-minute load (third loadavg field; the owner 2026-10-05: "It's twitchy, but you need to measure longer periods"; was the 5-minute load) and the `Avail` GiB of the data volume, not `/` (the sealed system volume always shows ~5%). With 16 cores: open below 16, hold 16-30, stop new above 30 (hysteresis, owner, 2026-10-01 18:4xZ: x1.5, 16/20, stop 30: from hold or stop open only below 16, from open hold only above 20, in between keep the previous band; `fleet-poll.py` applies it from the stored band); disk open at 200+ GiB free, hold 100-200, stop under 100.

The band itself is set in architect.md ("Builder slots follow CPU and disk"). The relay audience rule (who gets a band relay) is in architect.md too.

<!-- rule:r-108 -->
**Builder band command:**
```sh
sysctl -n hw.ncpu vm.loadavg; df -g /System/Volumes/Data | tail -1; memory_pressure | tail -1; sysctl -n vm.swapusage
```

## tmux priority (macOS)

<!-- rule:r-109 -->
`ps -o pri= -p "$(tmux display-message -p '#{pid}')"`. `4` means background QoS: every pane is limited to efficiency cores with throttled I/O. A tmux server started by a launchd job with `ProcessType Background` gets this, and it cannot be cleared at runtime. Only a new server started from a normal-priority context fixes it (`scripts/fleet-restart.sh`; trusty-tools #8415).

## Subsystem health

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

Record each check's figures in `local/state.md` under "Subsystem health" so the next check can compare.

<!-- rule:r-064 -->
- **Subsystem health.** Keep trusty-mpm and its subsystems, especially trusty-memory and trusty-search, at peak efficiency. On every full-poll pass (every 4th pass) and whenever a PM reports a tool failure, run the read-only checks listed below, compare results with the baseline in `local/state.md`, and act on a threshold crossing: non-destructive maintenance the user has authorized (listed in `local/state.md`) runs directly; anything else (deleting palaces, `--force` reindex of a populated index, daemon restarts) goes to the user with evidence and a recommendation. Framework defects follow the improvement pipeline.

## Build caches

<!-- rule:r-112 -->
**Rust gate rules (final, 2026-10-06; the owner "Fix code, rebuild till then").** Until #9298's follow-ups land everywhere: (1) every gate log shows `Compiling` (clippy: `Checking`) for EVERY changed crate in the dependency graph, from the gating worktree's path; a log without those lines does not count. (2) No `cargo clean` on any shared or exported target dir: the 07:46Z wording deleted ~190 GiB of other checkouts' output, and tm build-lease refuses it anyway. (3) Never gate two worktrees of one crate at the same time. (4) CI on fresh runners may replace the local test gate when local failures are explained by environment (tmux, long HOME) or by #9298; the Architect states it per PR. (5) A rustc or test binary at ~0 CPU for over 3 min is reported to the Architect by pid and never killed (owner e9: root spindump at the next one). (6) Stop a stalled agent before dispatching its replacement (two merge agents raced on 2026-10-06).

<!-- rule:r-113 -->
The shared build-cache pool is `~/.trusty-tools/cargo-target-pool/<org>/...` (2026-09-26: 810 GiB, almost all one org). Size it with `du -sh ~/.trusty-tools/cargo-target-pool/*`; a `find` over the home directory runs past 5 minutes.

<!-- rule:r-114 -->
Builder slots are seeded with `cp -c` (APFS copy-on-write clone of the shared target), so `du` over-counts slots that share blocks; judge cleanups by `df` before and after (2026-09-26, builder_slot_pool.rs).

## Secrets and backups

<!-- rule:r-115 -->
**Backing up secret-bearing files.** An Architect backup of a secret-bearing file (.env.local, tfvars) goes under `~/backups/` (dir 700, file 600), never beside the original inside a repo, where a broad `git add` could commit it; check `git check-ignore` if a backup must stay in place (2026-09-25 20:4xZ, a project .env.local).

<!-- rule:r-116 -->
**Secrets into env stores:** `security find-generic-password -w` ends its output with a newline. When piping it into `vercel env add` (or any store), strip it: `... -w | tr -d '\r\n' | vercel env add NAME production --sensitive`. A trailing newline made ElevenLabs reject a key with 401 (2026-09-26). Never echo the value; check the length only.
