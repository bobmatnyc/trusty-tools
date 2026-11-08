# ar-Tfd (df-G04 / df-T3b): keep trusty-analyze up — plan for the Architect
Read-only research, 2026-11-03. No build, install or launchctl change made.

## Recommendation (hybrid, opt-in KeepAlive; default binary behaviour unchanged)
Add a KeepAlive LaunchAgent that tctl writes, whose plist sets `TRUSTY_ANALYZE_IDLE_TIMEOUT_SECS=0`.
The idle knob already exists, so option B needs no new code path: only a plist env value.
Do NOT change the 600 s default: sandboxes, CI, `trusty-review` and every un-installed host keep on-demand behaviour.
Warning: this reverses the #6350 / ADR-0032 owner ruling of 2026-08-28 ("go on analyze and review"). It needs an explicit owner OK (D1).

## Findings
- Idle exit: `crates/trusty-common/src/uds/on_demand.rs:76` `DEFAULT_ANALYZE_IDLE_TIMEOUT = 600 s`; env `TRUSTY_ANALYZE_IDLE_TIMEOUT_SECS` (:66), `0` = never exit, parsed at :149 (`analyze_idle_timeout`).
- Applied by `serve_on_demand` (`crates/trusty-analyze/src/service/rpc.rs:526`), called from bare `serve` (`commands/run.rs:166`).
- Why: #6350 closed (ADR-0032:176-179). It removed the `KeepAlive: Always` unit because a resident process "nobody has spoken to" is what ADR-0032 retires; cold start re-opens facts redb + SCIP overlay + chunk caches.
- `serve --mcp` never idle-exits (`run.rs:134-150`, #6355): the stdio bridge owns the lifetime. Live now: pid 18584 `trusty-analyze mcp`, up 3 d, RSS 8.3 MB.
- Re-spawn today: `OnDemandAnalyze::ensure_running` (on_demand.rs:~150), detached supervisor, 20 s spawn probe (:130). Callers: trusty-review report adapter, `trusty-analyze deep`, tctl boot probe (`trusty-installer/.../probe_http.rs:1026`), console webhook (`trusty-console/src/webhook/spawn.rs:118`), `setup.rs:321`.
- Console detector never spawns it by design (`trusty-console/src/detect/analyze.rs:312`). trusty-search and tm do not spawn it (no `ensure_running` in either).
- Race arbiter: `bind_singleton_hardened`; a loser exits (on_demand.rs header). Under KeepAlive a loser is respawned every ThrottleInterval.
- Retired-unit plumbing: `trusty-common/src/launchd_labels.rs:109-115` (ANALYZE label), `:263-270` (in `RETIRED_SERVICES`); `trusty-installer/.../service_bootstrap.rs:233-235,250` (`member_has_service_install` = search|memory|console only); `trusty-analyze/src/commands/service.rs` + `main.rs:263-274` (`service uninstall` only).
- Shared pattern to reuse: `trusty_common::launchd::{LaunchdConfig, KeepAlive}` (`launchd.rs:37,76,214`); trusty-search uses `KeepAlive::Always`, log dir `~/Library/Logs/trusty-search` (`trusty-search/src/commands/service.rs:101,342`). Loaded today: search, memory, mpm, mpm.supervisor, console, events, agents.slack (`launchctl list`); no analyze.
- Why it goes down: no issue reports analyze down or slow cold start (gh search, 15 hits, none on analyze). The daemon is down by design after 600 s; the real cost is a cold start. Related: #8351 (bridge cannot recover when daemon down at handshake, closed), ar-Tfd C/a/b branches (bridge re-spawn, state marker, console states; see snapshot 2026-11-03T0600Z).
- Idle cost: embedder is `BowEmbedder` (fastembed removed, #5067); `ner` (ort) is an optional feature. Expect tens of MB RSS, no model memory in the default build. Measure D3.

## Options
| | A: KeepAlive plist | B: idle=0 default | Hybrid (rec.) |
|---|---|---|---|
| Idle cost | resident, few tens MB | same, on every host | resident only where installed |
| Crash restart | launchd | none until next client call | launchd |
| Install/uninstall | tctl writes/boots out plist; reverse retire | none | as A, opt-in flag |
| Sandbox_*.sh | plist is per-user; sandbox HOME differs, safe | sandbox daemons never exit; leak risk | safe |
| Multi-user/EVO | per-uid socket; one agent per user | same | per-user, opt-in |
| Code size | M (installer, labels, tests) | XS (one constant) | M |

## Change list
- `trusty-common`: `launchd_labels.rs` move ANALYZE from `RETIRED_SERVICES` to `SERVICES`; fix tests (`retired_analyze_*`).
- `trusty-installer`: `service_bootstrap.rs` `member_has_service_install` +trusty-analyze, plist builder (args `serve`, env idle=0, log dir `~/Library/Logs/trusty-analyze`, ThrottleInterval 10); stop evicting the unit.
- `trusty-analyze`: `commands/service.rs` + `main.rs:263`: add `service install|uninstall`, doc update; keep the 600 s default.
- `trusty-console`: `detect/analyze.rs` wording (resident is now a healthy state when installed).
- Docs: ADR-0032 amendment; fragments per crate (changelog.d).

## Test ladder and live check
- Rung 5 (process lifecycle, installer/launchd, supersedes a recorded ruling): rung 4 gates on trusty-common + each dependent, `--include-ignored`, failure-path tests, code-critic round.
- Live check (after Architect install clearance): `tctl install trusty-analyze`; `launchctl print gui/$UID/com.trusty.analyze` shows KeepAlive, running; idle >600 s, still up; `kill -9` -> back in <15 s; `trusty-analyze mcp` bridge still works; `service uninstall` removes it. Use plain `launchctl print`; no sandbox script covers analyze (none mention it).

## Open questions
- D1: owner OK to reverse #6350 / ADR-0032 for analyze? If no, ship option B alone via a per-host env.
- D2: opt-in flag (`tctl install --service analyze`) or on by default for tctl installs?
- D3: measure resident RSS of `serve` with idle=0 after a review run (facts + SCIP caches).
- D4: should ar-Tfd C/a/b (bridge re-spawn, state marker, console states) merge first? They stay valid under KeepAlive.
- D5: KeepAlive vs `serve --mcp` bridges: the bridge binds the same socket; confirm one-listener behaviour (bind loser exits, throttle loop).
