# Domain Consolidation Audit

> **The common-entry-point RULE is in [`CLAUDE.md`](../../CLAUDE.md)** (Key
> Conventions) and binds every change. This page is the dated status data behind
> it — consult it when you want to know whether a given domain has already been
> consolidated, not to decide whether the rule applies.

**Baseline audit 2026-07-11** (session d72fb4a3-f9ff-4394-b148-8200d17a2d5a). Update verdicts
as consolidations land. Reference: common-entry-point principle above.

| Domain | Verdict | Scale | Status |
|--------|---------|-------|--------|
| HTTP clients (reqwest) | FRAGMENTED | 150+ builder sites incl. 20+ inside trusty-common | backlog |
| git invocations | PARTIAL | `trusty_common::git::{command,command_in,tokio_command,tokio_command_in}`; ~26/90 production spawn sites migrated (2026-09-08, #7171) | follow-up: sweep the remaining ~50-60 sites below |
| Shared env-var access | FRAGMENTED | OPENROUTER_API_KEY ×22 files/8 crates; GITHUB_TOKEN ×13/6 | partially covered by epic #2400 (#2401) |
| gh CLI | CONSOLIDATED | `trusty_common::gh::GhCommand`; 15 production spawn sites across 4 crates migrated 2026-08-13 | done (#5475); the `tm ticket` / `tm watch` CLI keeps its injectable `CommandRunner` seam and passes `"gh"` as data — see below |
| Config-file loading | FRAGMENTED | 5 implementations (2 pairs within single crates) | backlog |
| Secret redaction | FRAGMENTED (security) | 4 rule sets (3 inside trusty-mpm) | partially covered by #2401 redact_secret |
| launchctl | SCATTERED | shared LaunchdConfig exists; 9 bypass sites/4 crates | backlog |
| Daemon addr discovery | SCATTERED | common resolver exists; 3 daemons re-implement | backlog |
| Daemon PID discovery | FRAGMENTED | 3 copy-pasted find_daemon_pids() | backlog |
| tmux (trusty-mpm) | SCATTERED→fixing | 19 sites | in flight: #2398/PR #2399 |
| LLM inference | FRAGMENTED→fixing | 6 bespoke clients | epic #2400 |
| Total-RAM detection for sizing | CONSOLIDATED | `trusty_common::machine_tier` (feature `machine-tier`); one production reader of `hw.memsize` / `/proc/meminfo` / the cgroup files, consumed by trusty-search and trusty-memory | done (#6820) — see below |

First enforcement instances: inference-adapter epic #2400, tmux common entry
point #2398/PR #2399.

## git invocations — what "PARTIAL" covers (#7171, 2026-09-08)

`trusty_common::git::{command, command_in, tokio_command, tokio_command_in}`
(unconditional — no feature gate) is the entry point, added to fix a fleet of
~25 worktrees each triggering an independent `git maintenance run --auto`
repack against the ONE shared object store they all point at (41 detached
repacks in one incident, load 141). Every command it builds carries
`-c maintenance.auto=false -c gc.auto=0` on argv, which outranks every config
file. `git grep -n 'Command::new("git")' -- crates/trusty-common/src
crates/trusty-mpm/src` still returns ~270 matches; the large majority are test
fixtures (`_tests.rs`, `tests.rs`, `#[cfg(test)] mod tests` bodies), which are
out of scope for this rule (#4058).

Migrated (production, non-test), ~26 sites: `daemon::managed_routes::inproject`
(clone + worktree-add), `inproject_hygiene` (fetch/merge/prune),
`inproject_start_point`, `inproject::untracked_sync`,
`core::session_launch::worktree_sync`, `core::session_launch::workstream_label`,
`provisioner::workspace::RealGitBackend::command` (cascades to clone/
remote_url/fetch_and_reset), `session_manager::worktree_safety::git_command`
(cascades to `core::harness_root`, `core::staged_paths`,
`session_manager::worktree_reconcile`, `session_manager::worktree_registry`,
`worktree_remove_command` and its callers in
`daemon::services::agent_worktree_reap` and `session_manager::decommission`),
`session_manager::decommission`'s own two `git worktree prune` sites, and
`bin/tm/formatters/info_box/probes`.

NOT migrated, follow-up needed (~50-60 sites): `trusty-common/src/catchup/*`
(behind the `catchup` feature — the lower-frequency DOC-28 session-catchup
bridge), `trusty-common/src/{github_path,palace_resolve,project_index_id,
repo_identity,credentials/dotenv}.rs` (identity/path resolution, mostly once
per session/startup), `trusty-mpm`'s `core::push_guard` / `bin/tm/commands/
push_guard.rs` (the `pre-push` hook install path, runs on `git push` not
fetch/commit), `bin/tm/commands/statusline/branch.rs` (a different spawn
mechanism — `core::spawn_disclaim::disclaimed_output`, string-dispatched for
TCC disclaim, not a `Command` builder to swap — and `git rev-parse` does not
touch the object store regardless), and roughly 20-25 remaining scattered
sites (`daemon::managed_routes::{deliverable_link,mcp_context,mcp_session}`,
`daemon::doctor_scaffold_tracking`, `core::instruction_pipeline`,
`core::project_aliases`, `session_manager::adopt`'s own call,
`bin/tm/commands::{daemon,guided,pr::open}`, `client::executor::managed`).

Before writing a new git-spawn helper, extend `trusty_common::git` rather than
inventing a second one — this row is now the "already consolidated,
mid-migration" case CLAUDE.md's common-entry-point rule exists to catch.

## Total-RAM detection — what "consolidated" covers (#6820)

`git grep -n 'sysctl\|total_memory\|hw.memsize\|MemTotal'` over `crates/**/*.rs`
returns exactly three production readers of memory, and they answer three
different questions. Only the first is machine-tier sizing:

| Site | Question | Verdict |
|---|---|---|
| `trusty_common::machine_tier::detect` | How much RAM may this process size itself for, at startup? | The single implementation. Reads `hw.memsize` / `/proc/meminfo` and clamps to the enclosing cgroup ceiling (#3657). trusty-search and trusty-memory both route through it; `trusty-search`'s `core::memory_policy` re-exports it so its own call sites are unchanged. |
| `trusty_common::host_metrics` (`sysinfo::total_memory`) | What is the host doing RIGHT NOW? | Not a duplicate. Live telemetry for the Foundry dashboard — total/used/available/swap with a pressure signal, sampled repeatedly. `sysinfo` applies NO cgroup clamp, so inside a capped container it reports the host's RAM; using it for sizing would reintroduce the #3657 bug. Keep separate. |
| `trusty-mpm`'s `doctor_pty_headroom` (`sysctl kern.tty.ptmx_max`) | How many PTYs may this machine allocate? | Not memory at all — matches the grep only on the word `sysctl`. |

No follow-up is owed. A fourth reader that needs a sizing number takes the
`machine-tier` feature rather than adding a fourth spelling.

## gh CLI — what "consolidated" covers (#5475, 2026-08-13)

`trusty_common::gh::GhCommand` (feature `gh-cli`) is the only place the string
`"gh"` is spelled as a program to spawn. It renders the argv, applies an
optional `--repo`, working directory, and environment overlay/removals, and
runs blocking, on tokio, or hands back the unspawned `std::process::Command`
for a caller that owns its own timeout. A non-zero exit is returned, never
raised — `gh pr checks` reports check state through its exit code — so each
call site keeps its own failure policy while sharing the spawn.

One family is deliberately NOT routed through it: the `tm ticket` and
`tm watch` commands in `crates/trusty-mpm/src/bin/tm/`, which call
`runner.run("gh", &args)` against an injectable `CommandRunner` trait. There
`"gh"` is DATA passed to a seam that exists so the tests can substitute a fake
process; replacing the seam with a concrete spawner would delete that test
coverage to satisfy a rule about spawn-site duplication. If those commands
ever need the entry point's classification, the right move is a `CommandRunner`
implementation backed by `GhCommand`, not a rewrite of the call sites.
