# trusty-computer Phase 2: build plan (Rust, bounded mode)

Plan only; no code, no build. Owner ruling: "Build Phase 2 with the bounded mode now." Start 2026-11-18T11:35Z. Sources:

- **Spike**: local branch `spike/trusty-computer` (947c76ee7b), `spikes/trusty-computer/` (GO-STOP.md, README.md, `*.py`).
  Local only; Phase 0 copies its evidence in. **Grants check**: branch `spike/trusty-computer-grants`.
- **Brief**: `architect/local/briefs/2026-11-17-trusty-computer-lift.md`.
- **Model crate**: `crates/trusty-browser/` 0.7.0 (~10.5k lines in `src/`), in worktree `agent-a2f985c7f624cb570`.
  Confirm it is on `main` before Phase 1 and re-cite that copy.
- **Actuals**: `docs/plans/trusty-browser-rust-port.md:125-144`.

## 1. Decisions

1. **Language: Rust**, new crate `crates/trusty-computer` (glob member `crates/*`; only `Cargo.lock` changes at the
   root). The owner moved trusty-browser to Rust, and the spike's Python is the behavioural oracle, ported case by
   case as in the browser port (port plan section 3, item 18). I found no strong reason against. Two cautions: the
   broker needs Darwin calls (`LOCAL_PEERCRED`) already written in `trusty-browser/src/peer.rs:24`, and the spike's
   51 pytest cases (`evidence/part2/pytest.txt`) are the first Rust test list.
2. **Bounded mode = caller-auth option (a)** (GO-STOP.md:113-115): daemon started with
   `--permission-mode bounded --capability-manifest <file>` on a private socket (`broker.py:237-239`). Option (c)
   (sandbox-exec, 2.5 d, GO-STOP.md:117) is not in the baseline; a 1-day proof of its LaunchServices rule is an
   optional end of Phase 7 (GO-STOP.md:120). The residual gap stays: the broker is a guardrail, not a security
   boundary (GO-STOP.md:11-12, 103-111). Bob must accept that in writing (GO-STOP.md:17).
3. **`invoke_menu` is denied** (GO-STOP.md:18). Every menu holds Restart, Shut Down, Log Out, and the spike rule
   checks only the pid (`policy.py:188-189`). A path allowlist is deferred.
4. **No screenshots**: `policy.py:138-142` stays. Phase 2 is AX-tree control only (brief risks 5 and 6).

## 2. Crate layout and reuse

Two bins as in trusty-browser (`Cargo.toml:25-31`): `trusty-computer` and `trusty-computer-mcp`. Each file stays
under the 500-line cap (`scripts/check_line_cap.sh`).

| Module | Job | Logic from |
|---|---|---|
| `policy.rs`, `policy_rules.rs` | 56-tool table, argument checks, `Scope` | `policy.py:161-249` |
| `redact.rs` | strip AXMenuBar elements and markdown, fingerprint | `redact.py:30-111` |
| `driver.rs` | NDJSON daemon client, `isError` mapping | `driver.py:45-86`, `broker.py:110-112` |
| `supervisor.rs`, `manifest.rs` | LaunchServices start, version pin, env, manifest render, restart | `driver.py:94-174` |
| `broker.rs`, `broker_call.rs` | sockets, call pipeline | `broker.py:79-167` |
| `peer.rs` | `LOCAL_PEERCRED` uid check | copy `trusty-browser/src/peer.rs:24-127` |
| `grants.rs`, `lease.rs`, `journal.rs` | grants, desktop lease with queue, `request_id` journal | pattern `grants.rs:38`, `leases.rs:22-35` |
| `audit.rs` | fail-closed JSONL, 0600, fsync, rotate | `audit.py:29-55`; pattern `audit.rs:96` |
| `operator.rs`, `consent.rs` | operator socket, mint flow, native dialog | pattern `cli_operator.rs:50-107` |
| `mcp_server.rs`, `mcp_tools.rs`, `mcp_session.rs` | stdio JSON-RPC, tool table, broker client | pattern `mcp_server.rs`; envelope from `trusty-mcp` |
| `cli.rs`, `launchd.rs`, `codesign.rs` | verbs, plist, Developer ID signing | copy and trim `launchd.rs:91-325`, `codesign.rs:84-198` |

**Recommendation: copy and trim now (about 0.5 d); extract a shared crate in a later PR.** Copy `peer.rs`,
`codesign.rs`, `launchd.rs`, the 0700/0600 helpers in `private.rs` and the audit shape. Do not copy `pyjson.rs`,
`casefold.rs`, `vault.rs`, `registry.rs`, `pairing.rs` or `keychain_migrate.rs`: they serve Python byte-parity and the
Chrome Keychain item, and trusty-computer has neither. Framing is newline-delimited JSON as in the spike, not
`frame.rs`. Cost of extracting first: about 2.5 engineer-days (20 h). It edits trusty-browser (rung 4-5, full parity
re-run) whose cutover was 2026-11-17 and whose soak is pending (port plan line 142). Copying duplicates about 650
lines. Each copied file carries a `// copied from trusty-browser <commit>` line so extraction is mechanical later.

## 3. Supervisor: bounded cua-driver daemon

Grants belong to `CuaDriver.app` (`com.trycua.driver`) and survive restart, full quit and relaunch
(GO-STOP.md:27-37). The daemon must start through LaunchServices to inherit them: `open -n -g -a CuaDriver --args
serve` (`driver.py:106-111`). The broker never execs the binary directly.

- **Pin:** `cua-driver --version` must equal `0.23.2` (`policy.py:18`) before every start, else refuse and audit
  `driver_version_mismatch`. Never call `update`, `check_for_update` or `skills install`.
- **Telemetry and update check off:** env `CUA_DRIVER_RS_TELEMETRY_ENABLED=false` and
  `CUA_DRIVER_RS_UPDATE_CHECK=false` on every start (README.md:38-45). The installer also writes
  `telemetry_enabled: false` and `update_check_enabled: false` into `~/.cua-driver/config.json`; `status` reports both.
- **Private socket:** `<state>/driver/cua-driver.sock`, dir 0700, socket 0600 (`broker.py:240-241`).
- **Overlay flag:** the +1 s bounded cost was not isolated from `--no-overlay` (GO-STOP.md:98-99); Phase 2 measures
  both and keeps the overlay if equal.

**The manifest pins exact pid and window (GO-STOP.md:140-142), so each scope change needs a restart.** Design:

1. **One manifest per lease, not per call.** The lease is single-holder, so one scope is live at a time. The
   supervisor renders the manifest from the holder's grant (apps, pids, window ids; tools from
   `fixture/capabilities.yaml:10-22` limited to the grant; `expires_after` = grant TTL, `idle_timeout` = lease idle
   TTL) and restarts only when the lease passes to a grant with a different scope. Restart time is unmeasured in the
   spike; Phase 2 times it.
2. **Rescope on drift.** A relaunched app or new window has a new pid or window id. When the daemon refuses a pid
   the broker's own `Scope` allows, the reply is `scope_stale`; the broker re-lists the grant's bundle id, rewrites
   the manifest, restarts once and retries read-only calls only. Mutating calls are never retried (section 4).
   `computer_refresh` lets the agent trigger it.
3. **Grants live broker-side.** The manifest is the daemon-side ceiling under the broker filter
   (`capabilities.yaml:1-3`). A grant naming an app with no live pid returns `app_not_running`.

Unverified, so the first live check: whether bounded mode admits a signed bundle by `bundle_id`
(GO-STOP.md:141-142). Test with a second signed fixture bundle, never a user app. If not, pid-only scoping ships.

**`session_ended`**: the daemon's implicit session ended and refused calls until `start_session` (GO-STOP.md:129).
The supervisor owns an explicit session: `start_session` after each daemon start and lease acquire; on a
session-ended error it starts a new one. Reads retry once. A mutating call returns `session_restarted`, the journal
entry is marked `outcome_unknown`, and it is not re-run. My sources do not give the exact error string; Phase 2
captures it from the live daemon into a fixture before writing the match.

## 4. Broker

Under `<state>` (default `~/.trusty/computer`, dir 0700). Sockets, all 0600 and uid-checked by `LOCAL_PEERCRED`
(`broker.py:46-52`, `194-209`): `agent.sock`, `operator.sock`, and the private `driver/` socket. A foreign uid gets
`foreign_uid` plus an audit line. `service.lock` flock keeps one broker per state dir (as `service.py:60-61`).

**Call pipeline** (port of `broker.py:79-126`; every refusal is audited):
1. `request_id` required; a repeat answers from the journal, audited `replayed` (`broker.py:82-87`). Journal is per
   grant, in memory, capped at 4096 (`leases.rs` `JOURNAL_LIMIT`). After a broker restart the journal is empty, so
   the agent must `computer_read` before retrying any mutating call.
2. Grant lookup (expiry, revocation), then lease check.
3. `check_call`: unknown tool denied, DENY refused, FILTER checks (`policy.py:234-249`). All 36 DENY and 14 FILTER
   rows port as-is; `invoke_menu` moves to DENY.
4. Token gate: an element call needs a token from the last redacted snapshot of that (pid, window)
   (`broker.py:99-102`, `128-138`); else `token_not_exposed`.
5. Audit `forward` line, then the daemon call; an audit failure refuses the call (`broker.py:103-106`).
6. `isError` becomes `ok:false, code: driver_refused` (`broker.py:110-112`).
7. `get_window_state`: drop every AXMenuBar element and its markdown subtree from structured and text parts, record
   `redacted: N`, store tokens and fingerprint (`redact.py:46-111`; 65 of 71 elements in the fixture, GO-STOP.md:62-71).
8. Click family: confirm by read-back, re-snapshot at once and after 250 ms; `confirmed` only if the redacted
   fingerprint changed; the driver's `unverifiable` never counts (`broker.py:140-167`, GO-STOP.md:76-87). An
   unconfirmed click returns "check, do not retry".
9. Audit `done` with `ok`, `ms`, `confirmed`: argument keys and text lengths only (`audit.py:29-36`).

**Grants.** `{id g-<12 hex>, bundle_id, tools subset, ttl, idle_ttl}`, scoped per app, minted only by the operator,
kept in 0600 `grants.json` so a broker restart does not drop them (a deliberate difference from trusty-browser,
whose grants are memory-only, port plan section on persisted state). Default tools: observe, click, scroll; `type_text`,
`set_value`, `press_key`, `hotkey`, `launch_app` need an explicit flag. Default TTL 30 min, idle 5 min. Browsers,
terminals, Finder, System Settings and password managers are refused at mint without `--allow-sensitive`, which is
audited (brief risk 6).

**One desktop lease.** One mutex across all grants and sessions with a FIFO queue (brief risk 3: 17+ sessions).
`computer_connect` returns `held` or `queued {position}`; it waits up to 20 s, then the client polls
`computer_status`. The holder keeps it until `computer_release`, grant end, peer disconnect (as `grants.rs:355`) or
120 s without a call. That is above trusty-browser's 60 s (`leases.rs:22`) because a confirmed click alone takes
about 3.2 s (GO-STOP.md:99-100). On release the next grant is promoted and the supervisor rescopes.

## 5. MCP tools (`trusty-computer-mcp`, stdio, 20 tools)

Action tools take `request_id` (optional in MCP, uuid4 filled; required on the wire). No `rmcp`; envelopes from `trusty-mcp`.

| Tool | One line |
|---|---|
| `computer_connect` | Redeem an operator code for a grant and ask for the desktop lease (held or queued). |
| `computer_status` | Lease state, queue position, grant TTL, driver version, telemetry state. |
| `computer_release` | Give up the lease for the next grant. |
| `computer_disconnect` | End the session and drop the grant binding. |
| `computer_grants` | Show this session's grant (app, tools, expiry); never other grants. |
| `computer_apps` / `computer_windows` | Running pids and windows of the granted app only. |
| `computer_read` | AX tree of a granted window, menu bar removed, element tokens included. |
| `computer_verify` | Check a predicate on window state. |
| `computer_click` | Click an element by token in background; returns `confirmed` by read-back. |
| `computer_double_click` / `computer_right_click` | As click, double or secondary button. |
| `computer_scroll` | Scroll an element by token. |
| `computer_type` | Insert up to 2000 chars into an element by token. |
| `computer_set_value` | Set an AX value on an element by token. |
| `computer_key` | One key to the granted app; quit, close, hide, switch chords refused. |
| `computer_hotkey` | A chord to the granted app, same refusals. |
| `computer_launch` | Background-launch the granted bundle id only. |
| `computer_wait` | Sleep up to 5 s in the broker; keeps the lease alive. |
| `computer_refresh` | Re-list pids and rescope after a relaunch or new window. |

Not exposed: `invoke_menu`, pixel tools, screenshots, clipboard, kill, focus, recording, config. A rung 6 test
asserts `tools/list` equals this table (20 tools) and each name maps to a non-DENY row.

## 6. Consent surface

Bob mints; an agent cannot mint for itself (brief lines 71, 109). Pattern: trusty-browser's operator socket and CLI
(`cli_operator.rs:50-107`).

- `trusty-computer grant <bundle-id> [--ttl 30m] [--tools ...]` goes to `operator.sock`. The broker opens a native
  confirm dialog from a separate helper process (`osascript display dialog`, owned by neither broker nor driver)
  showing app, tools, TTL and a six-digit code. The CLI reads that code from `/dev/tty` and mints a one-use grant
  code (`tc-` plus 120 random bits, 300 s, lockout after five bad redeems in 60 s, as the browser's codes, brief
  line 47). The agent redeems it with `computer_connect`.
- Why it holds: an agent can run the CLI in Bash (brief line 71) but has no `/dev/tty` and cannot read the dialog;
  `get_desktop_state` and screenshots are DENY (`policy.py:199`) and the bounded daemon refuses windows outside
  the manifest (GO-STOP.md:44-45).
- Residual gap: an agent with Bash can run its own `osascript` or `screencapture`. Only option (c) narrows that.
- `revoke`, `grants`, `status`, `stop` need no dialog; revoke ends the lease and audits `revoked`. Effort 1.5 d.

## 7. Install, launchd, signing

- `cargo install trusty-computer --version <v> --locked` (never `cp`, ADR-0043), then `trusty-computer
  install-service`: sign, write plist, bootstrap.
- Developer ID via `codesign.rs:105-198` (`detect_identity`, `ensure_signed`, `mcp_sibling`, `ensure_install_signed`),
  signing **both** bins; the browser port first missed the MCP sibling (port plan line 140). TCC attribution is
  `CuaDriver.app`'s, so there is no Keychain item and none of the dialog that cost 4.35 h of human wait in the
  browser cutover (port plan line 142). Still schedule a desk slot with a 20 min limit.
- Launchd label `com.trusty.computer`, `KeepAlive` (`launchd.rs:91-325`); the daemon starts on the first lease.
- `install-service` checks `/Applications/CuaDriver.app` is 0.23.2 and both grants are present via
  `check_permissions {"prompt": false}`; it never prompts (README.md:93-101 for Bob's grant steps).
- Isolated live runs use a new `scripts/sandbox_computer_daemon.sh` (temp state, no `.env.local`, no Keychain),
  per the CLAUDE.md hand-built-daemon rule (#9121). Built in Phase 6.

## 8. Tests

- **Unit, fake driver** (rung 3): port `tests/fake_driver.py` and the 51 cases (`test_broker.py`, `test_policy.py`,
  `test_redact.py`). Add lease queue order and expiry, journal replay, session-ended, rescope, grant expiry, audit
  write failure, operator lockout. A table test fails if any of the 56 tools in `evidence/list-tools-0.23.2.txt`
  lacks a policy row (README.md:48). Replay `evidence/part2/live-phases.jsonl` shapes against the fake driver.
- **Live, fixture only** (`#[ignore]`, `--include-ignored`, rung 5). The only app ever driven is the JXA Cocoa
  fixture (`grantcheck/fixture.js`, `make_app.sh`). The harness mints grants through a `test-seams` feature (as
  trusty-browser's `Cargo.toml` does), aborts unless the granted bundle id is the fixture's and the target pid is
  the fixture's, and refuses to run outside the sandbox script. No test lists, reads or scopes Bob's apps.
  Checks: grants survive initial, restart and full quit (GO-STOP.md:27-31); click leaves cursor and frontmost app
  unchanged; redaction (`redacted: N`, no "AXMenuBar"); confirmed versus inert button; daemon refuses
  `get_desktop_state`; a second client is queued while the first holds the lease; handoff rescopes; restart time;
  telemetry and update check read back as off; `invoke_menu` refused.
- **Owner conditions** (GO-STOP.md:15-19):
  1. *Typing interference* needs Bob at the keyboard. `grantcheck/typing_interference.py` activates a second
     fixture and uses System Events, which may raise an Automation prompt (GO-STOP.md:137-139). Book a desk slot
     before Phase 7, 20 min limit, run against the Rust broker. A failure adds an idle gate (refuse input while
     `CGEventSourceSecondsSinceLastEventType` shows input in the last 2 s), 1 d of contingency, not in the baseline.
  2. *One reboot check*, Bob present: after login with no manual step, `check_permissions` shows both grants and a
     fixture click lands (GO-STOP.md:136). The agent writes the checklist beforehand; the wait is the reboot.
  3. Caller-auth (a) ships; (c) optional. 4. `invoke_menu` denied: policy test plus live refusal.

## 9. Phases, estimates, rungs, time tracking

Engineer-day = 8 h. Total **12.5 engineer-days (100 h), range 10-16**: GO-STOP's 13.5 (11-17, GO-STOP.md:123-132)
minus option (c)'s 2.5 d, plus about 1.5 d for Rust (Darwin FFI; no Python oracle for the consent flow). **Agent
estimate: about 2.6 h wall-clock.** Basis: trusty-browser agent phases 0-6 took 2.15 h against 236 h estimated, about
0.9% (`trusty-browser-rust-port.md:129-138`). That held for byte-oracle translation; this crate has a live daemon,
signing and human gates, so I use 1% for translation and add live-check floors. Human waits are listed apart and are
not agent time. Phases 4 and 5 can run beside 3 after phase 1; the figures are serial.

| Ph | Work | Eng. d | Est. (h) | Agent est. (h) | Rung | Start (UTC) | End (UTC) | Actual (h) | Cause of gap |
|---|---|---|---|---|---|---|---|---|---|
| 0 | Skeleton, fake driver, spike cases and fixtures, sandbox script | 1.0 | 8 | 0.2 | 3 | 2026-11-18T11:40Z | 2026-11-18T11:50Z | 0.15 | Small spike oracle (~400 py lines), fast first build. Commit e2485b1f87 |
| 1 | Policy, redact, audit, journal | 1.5 | 12 | 0.25 | 3 | 2026-11-18T11:50Z | 2026-11-18T11:54Z | 0.07 | Module code written in phase 0 window; this window = red checks, gates. Commit e2677220d2 |
| 2 | Driver client, supervisor, manifest, pin, telemetry off, session, restart timing | 2.0 | 16 | 0.45 | 5 | 2026-11-18T11:55Z | 2026-11-18T12:09Z | 0.24 | Spike supplied protocol/flags/fake daemon. BLOCKED: live click fails, new private daemons see 0 AXWindows (ax_window_unresolved). Commit 8ea4806243 |
| 3 | Broker, peer auth, pipeline, grants, lease queue, read-back, rescope | 2.5 | 20 | 0.4 | 5 | 2026-11-18T12:14Z | 2026-11-18T12:41Z | 0.45 | Over agent est by 0.05h: 22-variant fail-open sweep (~15s builds) + one hung variant. Commit e04efed080 |
| 4 | Operator socket and CLI, helper dialog, code mint, lockout | 1.5 | 12 | 0.3 | 5 | 2026-11-18T12:42Z | 2026-11-18T12:55Z | 0.22 | Phase 3 rig and fake driver carried the flow; no live work. Commit 6587d809e5 |
| 5 | MCP server, 20 static tools, schema test, smoke run | 1.5 | 12 | 0.25 | 6 | 2026-11-18T12:55Z | 2026-11-18T12:59Z | 0.08 | Agent ops already in phase 4; smoke passed first run. Commit 45128b3755 |
| 6 | Install, launchd, signing both bins, README, fragment, doc gates | 1.0 | 8 | 0.2 | 5 | 2026-11-18T13:00Z | 2026-11-18T13:12Z | 0.20 | trusty-browser modules ported 1:1. check_permissions preflight not built (needs live daemon). Commit 92e81d23d1 |
| 7 | Live fixture checks, `code-critic` round, fixes | 1.5 | 12 | 0.5 | 5 | 2026-11-18T13:12Z | 2026-11-18T13:50Z | 0.63 | critic 0.08 (BLOCK, 2 HIGH); fix 1 0.26; delta 0.03 (WARN, 1 HIGH); fix 2 0.13 (9db1706942); delta 0.02 APPROVE; live 0.03; scans 0.02. Over agent est: two review/fix cycles; screen-lock wait not counted |
| | Total | 12.5 | 100 | 2.55 | | | | | |
| H | Human gates: typing test, reboot, signing desk slot, gap acceptance (about 1 h) | n/a | n/a | n/a | | | | | |
| P | This plan | 1 | 1 | n/a | | 2026-11-18T11:35Z | 2026-11-18T11:40Z | 0.08 | Over-estimate: sources already local |

**Gate commands** (CLAUDE.md; crate-scoped; `--no-fail-fast` is mandatory, #5354):
```bash
cargo fmt --check
cargo check -p trusty-computer
cargo clippy -p trusty-computer --all-targets -- -D warnings
cargo test -p trusty-computer --no-fail-fast
cargo test -p trusty-computer --no-fail-fast -- --include-ignored   # rung 5 live, sandbox script, Mac only
bash scripts/check_line_cap.sh && bash scripts/check_test_pointers.sh && bash scripts/check_sld.sh
bash scripts/check_changelog_fragment.sh --staged   # crates/trusty-computer/changelog.d/<n>-<slug>.md
```

Rungs: 0-1 rung 3. 2-4, 6, 7 rung 5 (lifecycle, security, release tooling): live and failure-path tests,
`code-critic` at Phase 7. Phase 5 rung 6 plus a stdio smoke run. No other crate changes. Live checks and signing run
on the Mac Studio; desktop-free builds may use EVO.

Owner questions: run the option (c) proof now? prefix `computer_*` or bare names (brief line 34)? add
`sandbox_computer_daemon.sh` (CLAUDE.md names five)?
