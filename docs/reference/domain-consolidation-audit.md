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
| git invocations | FRAGMENTED | ~90 production spawn sites, 7 crates | backlog (after tmux pattern proves) |
| Shared env-var access | FRAGMENTED | OPENROUTER_API_KEY ×22 files/8 crates; GITHUB_TOKEN ×13/6 | partially covered by epic #2400 (#2401) |
| gh CLI | FRAGMENTED | 3 wrappers (trusty-agents ticketing/gh_cli most complete) | backlog |
| Config-file loading | FRAGMENTED | 5 implementations (2 pairs within single crates) | backlog |
| Secret redaction | FRAGMENTED (security) | 4 rule sets (3 inside trusty-mpm) | partially covered by #2401 redact_secret |
| launchctl | SCATTERED | shared LaunchdConfig exists; 9 bypass sites/4 crates | backlog |
| Daemon addr discovery | SCATTERED | common resolver exists; 3 daemons re-implement | backlog |
| Daemon PID discovery | FRAGMENTED | 3 copy-pasted find_daemon_pids() | backlog |
| tmux (trusty-mpm) | SCATTERED→fixing | 19 sites | in flight: #2398/PR #2399 |
| LLM inference | FRAGMENTED→fixing | 6 bespoke clients | epic #2400 |

First enforcement instances: inference-adapter epic #2400, tmux common entry
point #2398/PR #2399.
