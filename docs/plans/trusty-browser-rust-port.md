# Port plan: trusty-browser (Python) to a Rust crate at 0.7.0

Status: plan only, no code. Date 2026-11-17. Author: research agent.

## 0. Source location and baseline

The source is in this repo, not a separate project. Commit `fb04ee366b` ("publish to PyPI at 0.7.0") sits on branch
`feat/trusty-browser-pypi-0.7.0`, checked out at
`$HOME/trusty-mpm-projects/bobmatnyc/trusty-tools/.claude/worktrees/agent-a40f304632fcbb299`. Package root:
`python/trusty-browser/` (`pyproject.toml`: name `trusty-browser`, version 0.7.0, Python >=3.12, deps `mcp`, `cryptography`, `keyring`).
Entry points: `trusty-browser = trusty_browser.cli:main`, `trusty-browser-mcp = trusty_browser.agent_mcp:main` (`pyproject.toml` `[project.scripts]`).
The spec of record is `python/trusty-browser/docs/activation-codes.md` (1133 lines; section map at lines 23-1119).
Nothing from the branch is on `main` yet, so the port starts from this branch, not `origin/main`.

## 1. Source inventory

Size (non-test): about 4,350 Python lines and about 1,660 extension JS lines. Tests: about 5,500 Python lines plus `test_worker.mjs`, `test_indicator.mjs`.

| Module (`src/trusty_browser/`) | Lines | Responsibility |
|---|---|---|
| `service.py` | 493 | Broker. Four Unix sockets, `service.lock` flock (`:60-61`), peer auth, route lookup (`:105`), pairing handshake `pair()` (`:130-165`), enrollment-socket dispatch (`:188-300`). |
| `protocol.py` | 252 | Frame codec, `Refusal`, `verify_peer`, `pairing_proof`, `Peer.command`/`Peer.page`, `MUTATING`, `PAGE_ERRORS`, `Claim`, `Session`. |
| `grants.py` | 431 | `connect`, `attach`, grant expiry/sweeper, `notify` of `grant_end`, operator ops. |
| `leases.py` / `pages.py` / `page_query.py` / `page_fields.py` | 177 / 524 / 115 / 60 | Tab leases (TTL 60 s, `leases.py:22`), page-op validation, journal for retries by `request_id`, query shaping. |
| `enrollment.py` | 469 | SQLite registry, workflow enrollment (legacy), `browser_key` (`:376`). |
| `pairing.py` | 153 | `pair_profile`, `unpair_profile`, same-email re-pair replacement (`:84-125`). |
| `vault.py` | 70 | AES-GCM sealing; wrapping key in macOS Keychain. |
| `audit.py` | 114 | Append-only `audit.jsonl`, 5 MiB rotate, keep 2, fail-closed. |
| `codes.py` | 60 | `Grant` dataclass, `RECONNECT_GRACE = 45.0` (`:16`). |
| `agent_mcp.py` / `page_tools.py` | 478 / 288 | MCP stdio server (FastMCP), `agent.sock` client `CodeSession`, upload-root checks (`:283-408`), page tools. |
| `native_host.py` / `mac_peer.py` | 100 / 133 | Chrome native-messaging relay to a socket; macOS peer PID/exe/argv/codesign checks (`mac_peer.py:91 verify_chrome_host`). |
| `install_host.py` / `cli.py` / `assets.py` | 77 / 218 / 141 | Host manifests and launchers; launchd plist and CLI verbs; extension publish into `state/extension`. |
| `extension/*` | 1,660 JS | MV3 extension. Shipped as data, not ported (section 2). |

### Persisted state (all under `--state`, default `~/.trusty/browser`, `cli.py:159`, dir mode 0700)

- `registry.sqlite3` (`enrollment.py:32-70`, 0600, `PRAGMA secure_delete=ON`). Tables: `routes`, `pending_credentials`, `revoked`, `pending_removals`, `route_versions`, and `pairings(profile TEXT PK, email TEXT UNIQUE, browser_key TEXT)`. `browser_key` holds a sealed value `tb1.` + urlsafe-base64(nonce12 || AES-256-GCM ciphertext+tag), AAD = `"browser:" + profile` (`vault.py:16,55-61`, `pairing.py:111-116`).
- The AES key is a Keychain generic password: service `com.trusty.browser.local-credentials`, account = `str(state.resolve())` (`vault.py:17,40`), base64 of 32 bytes. Serialised by `keychain.lock` flock.
- Grants are NOT persisted. `Grants.init_grants` keeps them in a dict (`grants.py:41-46`). They end when the browser closes (45 s grace) or the owner revokes. So "grants compatibility" means wire shape and audit lines, not a file format. Only pairings and the Keychain key must carry over.
- `audit.jsonl` (JSON lines `{"ts","event",...}`, field whitelist `audit.py:15-19`), `workflows/<name>.json` (legacy tickets), `extension/` (published assets with `version_name` stamp, `assets.py:21-79`), `assets/<hash>` symlinks, `service.lock`, `*.sock`, `chrome-native-host{,.enroll}` launchers, `service.std{out,err}.log`.
- Live Studio state today: `~/.trusty/browser/registry.sqlite3` holds 2 pairings and 0 routes (queried 2026-11-17).

### Wire protocol (no WebSocket, no HTTP)

1. Framing everywhere: 4-byte native-endian (`=I`) length + compact JSON, max 1 MiB, length 0 is invalid (`protocol.py:22-23,51-57`; same in `native_host.py:36-40`). On macOS/Linux "native" is little-endian in practice; keep `u32::from_ne_bytes`.
2. Chrome native messaging: extension <-> `native_host` stdio <-> `service.sock` (paired port) or `enrollment.sock`. Origin must equal `chrome-extension://jabfhcolknbbdajilgpficniedhcfahc/` (`native_host.py:14`). Host names `com.trusty.browser` and `com.trusty.browser.enroll` (`install_host.py:13,48`).
3. Pairing handshake on `service.sock` (`service.py:130-165`): `hello{type,profile,email,incarnation,nonce}` -> `challenge{nonce,proof}` -> `proof{proof}`. Proof = hex HMAC-SHA256 keyed by the ASCII browser key over `json.dumps([ "trusty-browser-native-v1", role, "chrome-extension://jabfhcolknbbdajilgpficniedhcfahc", profile, email, incarnation, client_nonce, server_nonce ], separators=(",",":"), ensure_ascii=False)` with roles `broker` and `extension` (`protocol.py:87-103`). The serialiser must reproduce Python's output exactly, including non-ASCII emails unescaped.
4. Broker -> worker commands `{"id","op","email","grant",...}` with 8 s wait (`protocol.py:187`), 29-ish page ops (activation-codes.md:438-555), refusal names in `PAGE_ERRORS` (`protocol.py:209`), mutating set `MUTATING` (`:192`) deciding `OutcomeUnknown` vs `BrowserDisconnected`.
5. `enrollment.sock` ops: `pair`, `unpair`, `connect`, `grants`, `revoke_grant(s)` (doc :149-237). `agent.sock`: `attach`, `profiles`, claim/page ops (doc :239-411). `operator.sock`: `grants`, `revoke_grant`, `revoke_grants`, `pairings`, `unpair` (doc :412-437; client `cli.py:116-135`).
6. Peer auth: `LOCAL_PEERCRED` uid on Darwin, `SO_PEERCRED` on Linux (`protocol.py:69-84`); enrollment additionally checks the host is Chrome's child, the launcher, not unsafe flags, codesign (`mac_peer.py:91+`, reasons in `audit.py:20-25`).
7. MCP side: stdio MCP via FastMCP (`agent_mcp.py:23,134,415`). Two modes: workflow mode (`--workflow`, credential file) and connect mode (`run_codes`, `agent_mcp.py:409`). Tools: `browser_claim/snapshot/click/release` plus `add_page_tools` (`page_tools.py:36-286`: tabs, select, switch, page_read/find/click/hover/drag/type/scroll/press/select/navigate/back/forward/reload/upload/downloads) plus connect-mode `browser_profiles/connect/disconnect` (`agent_mcp.py:417-440`).

### The "stale-worker" and `request_id` changes (fb04ee366b)

No commit message says "stale worker". The matching changes in that commit, which the port must keep:
- Stale extension worker handling: popup text "The Trusty Browser extension is out of date. Reload it at chrome://extensions." when the worker sends no reply (`extension/setup.js:29-37`); `trusty-browser extension-path` prints a reload notice on stderr while stdout stays the bare path (`cli.py:21-24,201-203`); the worker reloads when the broker reports a different `version_name` stamp (`assets.py:21-22`). Stale leases: `StaleLease` refusals (`leases.py:76-167`).
- `request_id` default: every MCP action tool takes `request_id: str | None = None`; `request_id_or_new` returns a fresh uuid4 when `None`, passes an explicit value (even empty) through (`page_tools.py:16-25`; `agent_mcp.py:63-86`).
- Broker side: a missing `request_id` is refused `InvalidRequest` with `"field":"request_id"`; so is an empty or non-string one (`protocol.py:30-48`; call site `leases.py:63-64`).
Decision for the Architect: confirm this reading (section 6, item 1).

## 2. Crate layout

One crate, `crates/trusty-browser` (crate name `trusty-browser`, directory equals name), version `0.7.0`, edition/MSRV from `[workspace.package]`. Library plus two bins in the same package:
- `src/main.rs` -> bin `trusty-browser` (clap subcommands: `serve`, `revoke`, `extension-path`, `install-host`, `launchd-plist`, `install-service`, `uninstall-service`, `grants`, `revoke-grant`, `revoke-grants`, `pairings`, `unpair`, plus `native-host`).
- `src/bin/trusty-browser-mcp.rs` -> bin `trusty-browser-mcp` (flags `--workflow`, credential flag, upload-root flag as in `agent_mcp.py:445-478`).
`native-host` is a new subcommand because there is no Python interpreter to run `native_host.py`. The launcher scripts and manifests keep their names; only the `exec` line changes (`install_host.py:49-51`).

Modules (each under the 500-SLOC cap, `scripts/check_line_cap.sh`; sizes are targets, Rust runs about 1.3x the Python):
`frame.rs` (codec, limits), `refusal.rs` (error names, details), `proof.rs` (transcript+HMAC), `peer.rs` (uid check; Darwin libproc/codesign in `peer_darwin.rs`), `vault.rs` (AES-GCM + Keychain), `registry.rs` (schema, open checks), `pairing.rs`, `audit.rs`, `grants.rs`, `leases.rs`, `pages.rs` (+ `pages_ops.rs`), `broker.rs` (accept loops, `broker_enroll.rs`, `broker_agent.rs`, `broker_operator.rs`), `native_host.rs`, `install_host.rs`, `launchd.rs`, `assets.rs`, `mcp_tools.rs` (+ `mcp_schema.rs`, `mcp_upload.rs`), `cli.rs`. About 24 files, about 6,000 SLOC. Tests under `tests/` (cap 3000 per file).

Extension: `extension/` copied byte-for-byte from the Python tree into `crates/trusty-browser/extension/` and embedded. Prefer `rust-embed` (already in workspace, `Cargo.toml:409`) over 17 hand `include_bytes!` lines; `include_dir` is the alternative. `assets.rs` must reproduce `signature()` (`assets.py:24-32`: sorted path order, `path\0` then sha256 of each file, sha256 over that) so the published `version_name` stamp equals the Python one for identical assets. Ordering is by path components, not by string.

Workspace deps reused: `tokio`, `serde`, `serde_json`, `clap`, `rusqlite` (bundled, `Cargo.toml:402`), `sha2` (`:274`), `hmac` (`:505`), `hex`, `base64`, `uuid` (v4), `libc`, `keyring` v3 with `apple-native` (`:330`), `tempfile`, `thiserror`/`anyhow`, `rust-embed`. New workspace entries: `aes-gcm` (already in `Cargo.lock` transitively, not a workspace dep) and optionally `plist` (or emit the plist as a fixed XML template to avoid it).
MCP: the workspace has no `rmcp`. The shared stdio JSON-RPC primitives are in `crates/trusty-mcp/src/lib.rs` (`run_stdio_loop`, `initialize` helper). Use `trusty-mcp` (lean, no `trusty-common` edge) and write the tool list as static JSON. Do not add `rmcp`.
Do not depend on `trusty-common`: the crate needs only its own state dir, and `trusty-common` is a heavy feature graph.

## 3. Parity test list

Fixture capture comes first (phase 0): run the Python 0.7.0 under a scratch `HOME` and temp state dir (never the live `~/.trusty/browser`) and store under `crates/trusty-browser/tests/fixtures/`.

Frame/handshake:
1. Frame codec: round-trip, size 0 and 1 MiB+1 refused `InvalidFrame`, non-object JSON refused, compact separators, `allow_nan=False` rejection.
2. `pairing_proof` known-answer vectors from Python for both roles, including a non-ASCII email and a 36-char profile; constant-time compare path.
3. Handshake end to end against a Python-written registry: hello field set exact, `ProfileMismatch`, `InvalidNonce` (nonce must be 64 lowercase hex), `proof_mismatch` audit line.
4. Each op in the page table (doc :438-555): request envelope, success envelope with `tab`, `IdentityMismatch`, `TabMismatch`, refusal-field whitelist (`tab,origin,access,document,typed,accepted,entered`, `protocol.py:200-206`), unknown error name becomes `BrowserRefused`.
5. Lost reply: `OutcomeUnknown` for every op in `MUTATING`, `BrowserDisconnected` for reads; worker-sent `OutcomeUnknown` journaled and a retry sends nothing (names in `protocol.py` docstring `:134-136`).
6. `enrollment.sock` ops: pair idempotent, `EnrollmentConflict` (`enroll_conflict_route`, `enroll_conflict_key`), same-email re-pair replaces the old row and ends its grants, unpair with wrong key, connect writes the audit line before the grant exists, `AuditUnavailable` fail-closed.
7. `agent.sock`: attach with 0, 1, many profiles (`NotConnected`, `ProfileChoiceRequired` with list), unknown extra fields -> `InvalidRequest`, grant ids `g-` + 12 hex, grace expiry at 45 s with an injectable clock, `grant_end` with `last`.
8. Peer refusals: wrong uid, non-Unix socket, each reason in `audit.py:20-25`.

State compatibility (the no-re-pair guarantee):
9. Read fixture `registry.sqlite3` made by Python: Rust opens it, lists the pairings, unseals each `browser_key` with the Python-created Keychain item, and answers a handshake the Python extension proof would satisfy. Run twice: fixture with a test Keychain service name, and one manual run against a copy of the live Studio state.
10. Reverse: a pairing written by Rust is readable by Python 0.7.0 (`tb1.` format, AAD `browser:<profile>`). Same for `audit.jsonl` lines and 5 MiB rotation names.
11. Schema: Rust must run the same `CREATE TABLE IF NOT EXISTS` set and the `route_versions` backfill (`enrollment.py:63-69`), and add no table or column.
12. Extension stamp: Rust `signature(extension/)` equals Python's for the same tree; `update()` writes manifest last and prunes stale files (`assets.py:61-90`).

Stale-worker and `request_id`:
13. Missing, empty or non-string `request_id` -> `{"ok":false,"error":"InvalidRequest","field":"request_id"}`; other `InvalidRequest` stays bare (`protocol.py:30-37`).
14. MCP: omitted `request_id` yields a fresh uuid4 per call (distinct across calls); explicit id is forwarded unchanged, an empty string is forwarded and refused by the broker; a repeat with the same id hits the journal.
15. `extension-path` prints only the path on stdout and the exact `RELOAD_NOTICE` on stderr (`cli.py:21-24`); `extension/setup.js` and `worker.js` are byte-identical to the Python tree (hash check), so the popup string and reload behaviour carry over. Rerun `tests/test_worker.mjs` and `test_indicator.mjs` unchanged against the embedded assets.

MCP schemas, byte compare:
16. Capture `tools/list` from the Python FastMCP server in workflow mode and connect mode (two fixtures) and the `initialize` result `serverInfo`. Assert Rust output equals the stored JSON byte for byte (tool order, descriptions, pydantic-style `title` keys, `anyOf [..,null]` for optional, defaults). Tool call results: success content shape and the error text `Error executing tool <tool>: <compact JSON frame>` (doc :938+). Upload root cases port from `tests/test_upload_root.py`.
17. CLI: `launchd-plist` output structure, exit codes and messages for operator verbs (`GRANT_ID`, `PROFILE_ID` regexes, `cli.py:101-106`), `Refusing to change an unrelated registration`.
18. Port the Python suites `test_activation_codes.py`, `test_pairing.py`, `test_enrollment.py`, `test_host.py`, `test_agent_mcp.py`, `test_page_ops.py`, `test_refusals.py` case by case. The Playwright-driven `test_*_dom.py` and `live_test.py` stay as Node/browser checks against the embedded extension and run against the Rust broker.

Gate rung: 5 (persistence, security, process lifecycle, release tooling) plus a code-critic round; rung 6 for the MCP schema surface.

## 4. Estimate (engineer-days)

| Phase | Work | Days |
|---|---|---|
| 0 | Fixture capture from Python 0.7.0 (registry, Keychain item under a test service, tools/list, proof vectors, audit lines); test harness | 2 |
| 1 | Crate skeleton, frame, refusal, proof, audit, registry, vault + Keychain read of the existing item | 4 |
| 2 | Broker: four sockets, peer auth (Darwin libproc, codesign), native-host subcommand, pairing, enrollment ops | 6 |
| 3 | Grants, leases, pages, journal, all page ops, `Peer::page` semantics | 6 |
| 4 | Extension embed, `assets.rs` publish and stamp, install-host, launchd, CLI verbs | 3 |
| 5 | MCP server both modes, static schemas, upload root | 4 |
| 6 | Parity suites, soak with two live Chrome profiles, flake runs | 4 |
| 7 | Release gates: changelog fragment, doc-comment pointers, `check_line_cap`, publish dry run, docs, Python deprecation notice | 2 |
| Total | | 31 (range 26-38) |

### Time tracking (owner ruling 2026-11-17T20:09Z; feeds a later mpm estimation skill)

Estimate in hours at 8 h per engineer-day. Actual hours are agent wall-clock time.

| Phase | Estimate (h) | Start (UTC) | End (UTC) | Actual (h) | Cause of gap |
|---|---|---|---|---|---|
| S Keychain spike | 4 | 2026-11-17T20:10Z | 2026-11-17T20:23Z | 0.22 | Over-estimate: fully scripted, prompts surfaced as timeouts, no human wait. Result: Python item ACL is cdhash-pinned; needs migrate-keychain handoff + Developer ID signing (spike 828b63cadb) |
| 0 | 16 | 2026-11-17T20:24Z | 2026-11-17T20:37Z | 0.20 | Over-estimate: scripted capture, warm shared target pool, no builder wait. ~2 min lost to a Keychain dialog incident (keyring macOS backend ignores keychain path; scratch HOME had no default keychain), fixed with a throwaway keychain + security CLI bridge. Commit d718385082 |
| 1 | 32 | 2026-11-17T20:38Z | 2026-11-17T20:52Z | 0.24 | Estimate predates phase 0 fixtures: translation against a byte-level oracle; spike removed Keychain risk; warm target, no builder wait. ~1 min lost to Write tool unescaping \u literals (#7480). Commit ae9ecd6e8b |
| 2 | 48 | 2026-11-17T20:53Z | 2026-11-17T21:16Z | 0.37 | Byte-level oracle with seeded randomness made it translation with fast checks; replay matched first full run; no builder/human wait. Commits bcf9fce1e0, af942b9e95. Peer-auth argv pins sent to security review |
| 3 | 48 | 2026-11-17T21:17Z | 2026-11-17T21:37Z | 0.34 | Byte-oracle translation, new page-op capture replayed first run; no waits. Dead workflow-session Leases.dispatch not ported (unreachable in 0.7.0). Commit 9464caadc9 |
| 4 | 24 | 2026-11-17T22:04Z | 2026-11-17T22:22Z | 0.30 | Python CLI as exact oracle; new tests green first run; no waits. Signing seam only (approval pending). Incident: Python install-service capture ran read-only `launchctl print` on real gui domain (no state change). Commit c4aff70614 |
| 5 | 32 | 2026-11-17T21:43Z | 2026-11-17T22:03Z | 0.33 | Python server as byte oracle via fake agent.sock capture; one fix (pydantic byte-length truncation); no waits. trusty_mcp stdio loop unusable (sorts keys); types only. Commits 416ee8b901, 59641d33f7 |
| 6 (non-live) | 32 | 2026-11-17T22:23Z | 2026-11-17T22:32Z | 0.15 | Suite already deterministic (0/90 flakes), warm slot; added smoke_e2e + test-seams serve switch. Live 2-profile soak deferred to cutover. Commit c49df6c20d |
| CR critic round | 2 | 2026-11-17T22:33Z | 2026-11-17T22:37Z | 0.06 | Read-only review; APPROVE, 2 MEDIUM + 3 LOW |
| FX fix round | 1 | 2026-11-17T22:36Z | 2026-11-17T22:41Z | 0.08 | Scoped fixes, warm cache. Commit 82d86cd37c |
| SEC flag fix | 1 | 2026-11-17T21:38Z | 2026-11-17T21:43Z | 0.08 | Small scoped fix, warm cache. Commit 0cb96b0009 |
| SG Developer ID signing | 2 | 2026-11-17T23:13Z | 2026-11-17T23:18Z | 0.10 | Seam + fake pattern already in place; real-sign test no dialog. Gap found: trusty-browser-mcp reads the Keychain item but is unsigned. Commit ef6eeca07b |
| SG2 sign mcp sibling | 1 | 2026-11-17T23:19Z | 2026-11-17T23:26Z | 0.10 | Small; workflow mode reads Keychain at start. Commit 4966d23f4b |
| CO cutover (steps 1-7) | 4 | 2026-11-17T23:26Z | 2026-11-18T03:57Z | 4.52 wall (0.17 agent + 4.35 human wait) | Unplanned Keychain dialog at migrate-keychain step (not at signing as planned), owner away; service outage 23:35-03:56Z. Lesson: dialog-capable steps need a scheduled desk slot and a wait limit. Soak pending |
| RD crate README + comparison | 3 | 2026-11-18T08:06Z | 2026-11-18T08:08Z | 0.04 | One-pass write from source; parallel source fetches; no hands-on competitor testing. Commit 275d713590 |
| SK two-profile soak | 4 | 2026-11-18T14:59Z | 2026-11-18T15:29Z | 0.50 (30 min soak by design) | Broker stable: pid unchanged, RSS/fd flat, 0 stderr, 330/330 session ops ok; page_read 0/60 PermissionNeeded (only tabs were ungranted new-tab pages) |
| 7 | 16 | 2026-11-17T22:41Z | 2026-11-17T22:45Z | 0.07 | Metadata/docs only; gates green first run. Push/PR/publish not yet counted. Commit bc066fedbe |

Single engineer, serial. Phases 3 and 5 can run in parallel with two engineers (about 25 calendar days).

## 5. Cutover on the Studio

How Studio runs it today (this machine, `terraces-mac-studio`):
- launchd job `com.trusty.browser`, `~/Library/LaunchAgents/com.trusty.browser.plist`: `ProgramArguments` = uv venv `python` + `.../site-packages/trusty_browser/service.py --state $HOME/.trusty/browser serve`, `RunAtLoad`, `KeepAlive`, `Umask 63`, logs in the state dir.
- Install by `uv tool install` at `~/.local/share/uv/tools/trusty-browser`; shims `~/.local/bin/trusty-browser` and `~/.local/bin/trusty-browser-mcp`.
- State `~/.trusty/browser` (files listed in section 1). Prior install records and backups: `~/.trusty/browser.bak-20261115*` and `*.INSTALL.txt`.
- Chrome native host manifests in `~/Library/Application Support/Google/Chrome/NativeMessagingHosts/` (`install_host.py:57-63`), launchers in the state dir run the venv python on `native_host.py`.

Why the Python `install-service` cannot be reused for the swap: it refuses any plist or launcher whose content differs from what it would write (`cli.py:55-65`, `install_host.py:20-25`), so the Rust binary must replace the old registration through its own `uninstall-service` then `install-service`, written to accept the old Python content as "ours" once.

Steps:
1. Release the crate (`cargo publish` is delegated to `local-ops`). Install with `cargo install trusty-browser --version 0.7.0 --locked` run outside the workspace, never `cp` (CLAUDE.md macOS rule). Install lands in `~/.cargo/bin`, so it does not touch the uv shims yet.
2. Back up: `cp -Rp ~/.trusty/browser ~/.trusty/browser.bak-<date>`; copy the plist to `com.trusty.browser.plist.bak-<date>`; copy the two Chrome manifests; record `launchctl print gui/$UID/com.trusty.browser` (pid, path) in an `INSTALL.txt` as earlier installs did.
3. Dry check, no state change: with a scratch state dir and test Keychain service, run `trusty-browser serve`; then run the Rust binary read-only against a copy of the backup to confirm pairings unseal.
4. Keychain access: run the Rust binary once interactively in the GUI session against the real state to grant the Keychain ACL (the item was created by the uv python, so a new binary can trigger a prompt; a launchd job cannot answer it). Confirm no prompt on a second run.
5. Swap: `launchctl bootout gui/$UID/com.trusty.browser`; `uv tool uninstall trusty-browser` is left until step 8; run the Rust `trusty-browser install-service`, which rewrites the plist (`ProgramArguments` = the cargo-installed binary, `serve`), both launchers, and both manifests, then bootstraps.
6. Reload: `trusty-browser extension-path`, then reload the extension in each Chrome profile (stale-worker notice applies; the stamp is unchanged if assets are byte-identical, so a reload may not be needed, but do it once). No re-pairing step is expected; verify with `trusty-browser pairings` showing the same 2 profile ids.
7. Verify: audit.jsonl shows a handshake with no `proof_mismatch`; popup Connect works in both profiles; `browser_profiles` then `browser_connect` through `trusty-browser-mcp` and one `page_read`. Point MCP client config at `~/.cargo/bin/trusty-browser-mcp` (or put it ahead of the uv shim on PATH).
8. After 7 days clean: `uv tool uninstall trusty-browser`, mark the PyPI project deprecated if it was ever published, keep the Python tree frozen under `python/trusty-browser/` until the removal PR.

Rollback (valid at any step because the Rust build adds no schema or file-format change):
1. `launchctl bootout gui/$UID/com.trusty.browser`; `trusty-browser uninstall-service` (Rust).
2. Restore the saved plist and the two launchers and manifests; if state was touched, `mv` the `.bak` directory back (stop the service first so `service.lock` is free).
3. `launchctl bootstrap gui/$UID ~/Library/LaunchAgents/com.trusty.browser.plist`; reload the extension; `trusty-browser pairings` (Python) lists the same profiles.
4. Repoint MCP config at the uv shim. The two brokers must never run together: both take the `service.lock` flock (`service.py:60-61`), so the second exits.

## 6. Open risks and decisions for the Architect

1. Meaning of "stale-worker fixes". I found no commit by that name. I mapped it to the popup out-of-date message, the reload notice, `version_name` reload, and `StaleLease`. Confirm, or name the commit or issue.
2. Keychain ACL (highest risk). The wrapping key is owned by the uv python in the login Keychain. A different binary may be denied or prompt, and under launchd a prompt means a hung or failed service. Options: one-time interactive "Always Allow" (step 4), or a one-shot `migrate-keychain` that rewrites the item for the new binary. Needs a spike in phase 1 before anything else. A ad-hoc-signed `cargo install` binary also changes identity on every reinstall, which can re-trigger the prompt; decide whether to codesign with a stable identity.
3. `keyring` crate v3 vs Python `keyring` 25 on the same item: both address service/account generic passwords, but attribute handling must be proven by fixture 9, not assumed. Fallback: call Security.framework directly.
4. Enrollment-socket host verification (`native_host.py:51-64`, `mac_peer.py:91+`) pins the service's argv to `python service.py --state <dir> serve` and compares interpreters. That logic has no Rust analogue as written. Define the replacement (peer exe equals the installed `trusty-browser` path and argv `serve --state`), and note it is a security control, so it needs the code-critic round and a security review.
5. MCP schema byte parity with FastMCP/pydantic output (titles, `anyOf`, ordering) is brittle. Plan: static JSON generated once from Python and asserted. If the owner accepts semantic equivalence instead of bytes, the schemas can come from `schemars`. Also decide the MCP protocol version string the Rust `initialize` returns; clients may negotiate.
6. crates.io: confirm the name `trusty-browser` is free or owned, and that a first publish at 0.7.0 (no 0.1-0.6 on crates.io) passes `preflight-publish.sh` and the semver gate. Was `trusty-browser-v0.7.0` ever tagged for PyPI? If yes, retire `publish-trusty-browser.yml` and `.github/workflows/trusty-browser.yml` (CI runs pytest/playwright) in the cutover PR; if not, drop them unmerged.
7. Directory: the crate lives in `crates/trusty-browser`; `python/trusty-browser` is deleted only after the 7-day soak. The extension has two copies until then; the extension in the crate must stay byte-identical, so add a CI hash check.
8. Platform: the Python vault is macOS only (`vault.py:22-23`) while peer auth supports Linux. Keep macOS-only for 0.7.0 (matches today) or add a Linux secret store. Recommend macOS-only; EVO can gate non-Mac code compile only.
9. Playwright live and DOM tests need Python or Node. Keep them as a CI job that points at the Rust binary, or port to Node. Recommend keep.
10. JSON fidelity: Python `json.dumps` defaults (`ensure_ascii`, float formatting, key order) feed the HMAC transcript and audit lines. Use `serde_json` with `preserve_order`, and a custom transcript serialiser covered by fixtures 2 and 10.
11. 5 MiB audit rotate and `keep 2` naming must match so a rollback finds the files it expects (`audit.py:26-27`; rotate body not read in this pass).
