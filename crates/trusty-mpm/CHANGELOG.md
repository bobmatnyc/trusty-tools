# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---
## [Unreleased]

### Added

- orphan-GC PID registry + PR A nits ([#1595](https://github.com/bobmatnyc/trusty-tools/pull/1595)) ([#1637](https://github.com/bobmatnyc/trusty-tools/pull/1637)) ([`3886d33`](https://github.com/bobmatnyc/trusty-tools/commit/3886d33e077240bac3e5417427818c41a66e5d8b))
- WI-4 PR A — graceful shutdown hardening (refs #1595) ([#1617](https://github.com/bobmatnyc/trusty-tools/pull/1617)) ([`7f5ed43`](https://github.com/bobmatnyc/trusty-tools/commit/7f5ed43a646fbb1c67f2eb176703a11f696e0712))
- WI-3 SESSCTL Phase 3 — activity observability (closes #1594) ([#1600](https://github.com/bobmatnyc/trusty-tools/pull/1600)) ([`36aebaf`](https://github.com/bobmatnyc/trusty-tools/commit/36aebaf9a43d117dc7441253a52d0b648f00487e))
- WI-2 SESSCTL Phase 2 — sessctl command surface + daemon HTTP endpoints (closes #1593) ([#1599](https://github.com/bobmatnyc/trusty-tools/pull/1599)) ([`3647649`](https://github.com/bobmatnyc/trusty-tools/commit/3647649eb824ff26cdc3524ac89a1004b9e1f9f4))
- WI-1 SESSCTL Phase 1 — backend trait + SessionActor + registry foundation (closes #1592) ([#1598](https://github.com/bobmatnyc/trusty-tools/pull/1598)) ([`68d102a`](https://github.com/bobmatnyc/trusty-tools/commit/68d102a9dc8706a59120923c55bbcbca13e0dae6))
- WI-B group /fleet output by project ([#1588](https://github.com/bobmatnyc/trusty-tools/pull/1588)) ([`9d88c33`](https://github.com/bobmatnyc/trusty-tools/commit/9d88c33880f51d2f857b92f6fad7526bc4ea3c1d))
- WI-A thread repo_url/ref_ through LaunchParams + sessions.launch ([#1587](https://github.com/bobmatnyc/trusty-tools/pull/1587)) ([`64ba815`](https://github.com/bobmatnyc/trusty-tools/commit/64ba815aaf3976e87c686a4e49c8c8ff26833ccb))
- WI-1 isolation regression-guard with version capture (closes #1582, refs #1548) ([#1583](https://github.com/bobmatnyc/trusty-tools/pull/1583)) ([`23d846c`](https://github.com/bobmatnyc/trusty-tools/commit/23d846c35058128d953f71c9aaf4933e1630d67d))
- output-style filesystem deployer for managed config (closes #1553) ([#1580](https://github.com/bobmatnyc/trusty-tools/pull/1580)) ([`46e6f40`](https://github.com/bobmatnyc/trusty-tools/commit/46e6f4092606c98aed2f32459b99cbb28cc5557b))
- tm update + tm rm standalone lifecycle subcommands ([#1578](https://github.com/bobmatnyc/trusty-tools/pull/1578)) ([`b5e4b20`](https://github.com/bobmatnyc/trusty-tools/commit/b5e4b20378d122318bb8bb690911b8c51512d571))
- configurable managed-root via --root / TRUSTY_MPM_ROOT / config.toml ([#1567](https://github.com/bobmatnyc/trusty-tools/pull/1567)) ([`7f781e0`](https://github.com/bobmatnyc/trusty-tools/commit/7f781e0de2e8f9bec2863998f1c0b664c1393c37))
- WI-8 wire trusty-review MCP into tm-global managed config (refs #1548) ([#1563](https://github.com/bobmatnyc/trusty-tools/pull/1563)) ([`a4f1805`](https://github.com/bobmatnyc/trusty-tools/commit/a4f18051a8a64851b37244afda2bbb2fa002c1f0))
- WI-3 managed-session hook-clean + trust-seed + MCP-enable (refs #1548) ([#1555](https://github.com/bobmatnyc/trusty-tools/pull/1555)) ([`66ee38a`](https://github.com/bobmatnyc/trusty-tools/commit/66ee38a58d25e915d7eb98448961b45b42eca390))
- WI-2 deploy bundled agents+skills into managed CLAUDE_CONFIG_DIR (refs #1548) ([#1552](https://github.com/bobmatnyc/trusty-tools/pull/1552)) ([`bce34bd`](https://github.com/bobmatnyc/trusty-tools/commit/bce34bdec7d522d47985104bc0509ced681fbfe1))
- WI-10 managed-session auth — tm login (keychain) + ANTHROPIC_API_KEY/--bare fallback (refs #1548) ([#1551](https://github.com/bobmatnyc/trusty-tools/pull/1551)) ([`539c94a`](https://github.com/bobmatnyc/trusty-tools/commit/539c94ac89a0f38a23b0db9aee9af902d6f89690))
- MVP standalone managed driver — register/load/run with CLAUDE_CONFIG_DIR isolation (refs #1548) ([#1549](https://github.com/bobmatnyc/trusty-tools/pull/1549)) ([`81ca1b0`](https://github.com/bobmatnyc/trusty-tools/commit/81ca1b0dda406113469336f3c492933d07f3bf94))
- NL->repo resolver (WI-5, refs #1517) ([#1535](https://github.com/bobmatnyc/trusty-tools/pull/1535)) ([`222d638`](https://github.com/bobmatnyc/trusty-tools/commit/222d638e6a3f2a11b5d1939299c5a31a3b063904))
- project registry + MCP tools (closes #1519) ([#1520](https://github.com/bobmatnyc/trusty-tools/pull/1520)) ([`53f95c2`](https://github.com/bobmatnyc/trusty-tools/commit/53f95c2d61c1522adfec7e90171187d39523e578))
- wire decommission/inject verbs + graceful no-creds path in action coordinator (closes #1524) ([#1525](https://github.com/bobmatnyc/trusty-tools/pull/1525)) ([`53c49dc`](https://github.com/bobmatnyc/trusty-tools/commit/53c49dc6936c6afa8443e480c1adbc1a399f431b))
- harness-understanding instructions in trusty-agents-common + DOC-21 (closes #1510) ([#1513](https://github.com/bobmatnyc/trusty-tools/pull/1513)) ([`737cddb`](https://github.com/bobmatnyc/trusty-tools/commit/737cddbb6e8908a268604f74a41f361e13f431fc))
- track & tear down ephemeral managed sessions (closes #1508) ([#1509](https://github.com/bobmatnyc/trusty-tools/pull/1509)) ([`3b7d0c9`](https://github.com/bobmatnyc/trusty-tools/commit/3b7d0c9f1225b989687f869b7158bd83b653fc70))
- Slack adapter on the chat-core seam (#1294, epic #1433) ([#1504](https://github.com/bobmatnyc/trusty-tools/pull/1504)) ([`330a2d9`](https://github.com/bobmatnyc/trusty-tools/commit/330a2d918f597b7b91da5aec9d0a9879ec5e5aef))
- web adapter on chat-core seam (refs #1433, #1295, #926) ([#1503](https://github.com/bobmatnyc/trusty-tools/pull/1503)) ([`1c0ccdf`](https://github.com/bobmatnyc/trusty-tools/commit/1c0ccdf93269cd45482a8cb76268c35bfee3247f))
- adopt existing tmux sessions + local-path managed spawn (refs #1433) ([#1502](https://github.com/bobmatnyc/trusty-tools/pull/1502)) ([`be25fff`](https://github.com/bobmatnyc/trusty-tools/commit/be25fff3aa2effdd1f462d2b670ae84e70daf973))
- drive managed fleet from Telegram — free-text→action chat + managed slash commands ([#1501](https://github.com/bobmatnyc/trusty-tools/pull/1501)) ([`5bd5c55`](https://github.com/bobmatnyc/trusty-tools/commit/5bd5c55e985df25049967334b8d3c9cfd0828540))
- add health verb to chat-core catalog + action loop (refs #1433) ([#1498](https://github.com/bobmatnyc/trusty-tools/pull/1498)) ([`5f7b526`](https://github.com/bobmatnyc/trusty-tools/commit/5f7b526982738cc6776792037ac83f9f70be7e72))
- action-capable coordinator chat — self-aware inline verb execution ([#1496](https://github.com/bobmatnyc/trusty-tools/pull/1496)) ([`5c792e7`](https://github.com/bobmatnyc/trusty-tools/commit/5c792e7e8637bd003dbb7bfcc277f263ff4d4008))
- wire STUI slash-dispatch + free-text routing through chat-core (refs #1272, #1276) ([#1494](https://github.com/bobmatnyc/trusty-tools/pull/1494)) ([`62bb3c1`](https://github.com/bobmatnyc/trusty-tools/commit/62bb3c15cf26ee2a6fc3ae2c3d7a0d9df4899273))
- route tm CLI session verbs through chat-core; drop duplicate resolvers (refs #1283) ([#1493](https://github.com/bobmatnyc/trusty-tools/pull/1493)) ([`586f2eb`](https://github.com/bobmatnyc/trusty-tools/commit/586f2eb3032979923869441e1335e0c49a82cdd0))
- chat-core nucleus — shared command layer for session-manager adapters ([#1492](https://github.com/bobmatnyc/trusty-tools/pull/1492)) ([`baaf568`](https://github.com/bobmatnyc/trusty-tools/commit/baaf5689603be72fe7625747c76fddc488d33958))
- typed DaemonClient managed-session methods + refactor tm managed cmds ([#1491](https://github.com/bobmatnyc/trusty-tools/pull/1491)) ([`c5287af`](https://github.com/bobmatnyc/trusty-tools/commit/c5287af26a98b62376914445b1853052f2c0cd6b))
- meta run launches a real Claude Code session + verifies demo artifact (closes #1049, closes #1051) ([#1489](https://github.com/bobmatnyc/trusty-tools/pull/1489)) ([`26fbf15`](https://github.com/bobmatnyc/trusty-tools/commit/26fbf15284136569190796b390597342f9afa717))
- custom-instruction loading for the metaharness (closes #1048) ([#1485](https://github.com/bobmatnyc/trusty-tools/pull/1485)) ([`ee2c498`](https://github.com/bobmatnyc/trusty-tools/commit/ee2c498253f0ec5e9f9d9a25c5f89e9a14d35d9a))
- STUI-1 numbered scrollable session list + keybindings + state preservation (refs #1278) ([#1482](https://github.com/bobmatnyc/trusty-tools/pull/1482)) ([`5bf0009`](https://github.com/bobmatnyc/trusty-tools/commit/5bf00095aef20380f6b32aeb4604ceadbbe41e18))
- standard harness-agnostic inject_text/observe/summarize on SessionControl ([#1461](https://github.com/bobmatnyc/trusty-tools/pull/1461)) ([#1463](https://github.com/bobmatnyc/trusty-tools/pull/1463)) ([`967c892`](https://github.com/bobmatnyc/trusty-tools/commit/967c892236105928ab03b25080cd293392673299))
- periodic + startup orphan-GC reconciling registries vs tmux ls ([#1458](https://github.com/bobmatnyc/trusty-tools/pull/1458)) ([#1462](https://github.com/bobmatnyc/trusty-tools/pull/1462)) ([`aa1c8f8`](https://github.com/bobmatnyc/trusty-tools/commit/aa1c8f8cee680648f8cd5231778f5bbb87a5308d))
- owning tmux Session guard with RAII Drop reaper + test teardown guards (refs #1453, #1459, epic #1452) ([#1460](https://github.com/bobmatnyc/trusty-tools/pull/1460)) ([`ff25808`](https://github.com/bobmatnyc/trusty-tools/commit/ff25808cf8fcb682851d6ba376501cece5674e6f))
- sessions TUI startup banner + service probes (STUI-0) ([#1431](https://github.com/bobmatnyc/trusty-tools/pull/1431)) ([`734b5e5`](https://github.com/bobmatnyc/trusty-tools/commit/734b5e54f2688052cadb2f5e3c26f8ab2d09b139))
- coordinator-context last_summary + summarizing flag (STUI-4) ([#1432](https://github.com/bobmatnyc/trusty-tools/pull/1432)) ([`0fda534`](https://github.com/bobmatnyc/trusty-tools/commit/0fda534e6359d8963ad59044421d4a6a82822afd))
- catalog update-check + rebuild/apply (closes #1408) ([#1429](https://github.com/bobmatnyc/trusty-tools/pull/1429)) ([`41b312a`](https://github.com/bobmatnyc/trusty-tools/commit/41b312a5cb78c469e9c3b0968107c2fd90340203))
- manifest-driven harness provisioning (HR-2, #1407) ([#1427](https://github.com/bobmatnyc/trusty-tools/pull/1427)) ([`f012658`](https://github.com/bobmatnyc/trusty-tools/commit/f012658d26560735b2659a4649cabff41b4ebac8))
- multi-style output + version-fallback injection (HR-4) ([#1412](https://github.com/bobmatnyc/trusty-tools/pull/1412)) ([`77ad339`](https://github.com/bobmatnyc/trusty-tools/commit/77ad33964556158f9816cf9e0fd7de7967ee9114))
- BASE agent content parity + initialPrompt/tier-model injection ([#1411](https://github.com/bobmatnyc/trusty-tools/pull/1411)) ([`0a28b24`](https://github.com/bobmatnyc/trusty-tools/commit/0a28b24550e041139b59c79daead1da3671d1f29))
- wire in-process AgentRunner into meta run orchestrator (closes #1030) ([#1396](https://github.com/bobmatnyc/trusty-tools/pull/1396)) ([`19b972d`](https://github.com/bobmatnyc/trusty-tools/commit/19b972d6e6158ebbc0013a59561c724226d2213a))
- coordinator TUI live session-list polling (Child #2, refs #1274) ([#1386](https://github.com/bobmatnyc/trusty-tools/pull/1386)) ([`44345df`](https://github.com/bobmatnyc/trusty-tools/commit/44345df721f9af648b0b9ac5bdc22d2ee1bcc5dc))
- coordinator TUI skeleton screen + tm coordinator-tui subcommand (Child #1, refs #1272) ([#1383](https://github.com/bobmatnyc/trusty-tools/pull/1383)) ([`2dab8d4`](https://github.com/bobmatnyc/trusty-tools/commit/2dab8d4588a139b7179716f25d8e657548cc5a72))
- wire trusty-code ToolRegistry into tm meta run (WI-2, refs #1045) ([#1384](https://github.com/bobmatnyc/trusty-tools/pull/1384)) ([`0684a7d`](https://github.com/bobmatnyc/trusty-tools/commit/0684a7d2477716904077a7381745f220b5e5c1ed))
- bootstrap tm meta run subcommand (WI-1, refs #1045) ([#1382](https://github.com/bobmatnyc/trusty-tools/pull/1382)) ([`cdaa6c7`](https://github.com/bobmatnyc/trusty-tools/commit/cdaa6c728a7d491984c32fbd27c33e64163b92c9))

### Fixed

- repair daemon::state overseer tests and daemon::api blocking-client panic (closes #1571, closes #1523) ([#1581](https://github.com/bobmatnyc/trusty-tools/pull/1581)) ([`216ee11`](https://github.com/bobmatnyc/trusty-tools/commit/216ee1144ff3173ed5fb59b37383cef10daa30c7))
- managed MVP polish — atomic-save cleanup, credential direction, idempotent .mcp.json ([#1579](https://github.com/bobmatnyc/trusty-tools/pull/1579)) ([`e1721ad`](https://github.com/bobmatnyc/trusty-tools/commit/e1721ad6943f6a32a52a7839aad16c01634cd1de))
- atomic confirm_pair_code with crash-safe claim cleanup (closes #1506) ([#1547](https://github.com/bobmatnyc/trusty-tools/pull/1547)) ([`731d915`](https://github.com/bobmatnyc/trusty-tools/commit/731d91511327124eb795fea5c6ffdfc31db18427))
- stop dropping tokio Runtime in async test context (closes #1521) ([#1522](https://github.com/bobmatnyc/trusty-tools/pull/1522)) ([`ac9365b`](https://github.com/bobmatnyc/trusty-tools/commit/ac9365b3a511ea72fd796ccb6c4e6aafb5dbd25c))
- HTML-escape Telegram command replies (closes #1514) ([#1515](https://github.com/bobmatnyc/trusty-tools/pull/1515)) ([`0e577e3`](https://github.com/bobmatnyc/trusty-tools/commit/0e577e3ab7ff7424e5a5a6242c4094e017170309))
- guard decommission against deleting non-owned workspaces (P0, closes #1511) ([#1512](https://github.com/bobmatnyc/trusty-tools/pull/1512)) ([`435a962`](https://github.com/bobmatnyc/trusty-tools/commit/435a962fc5ae631d5d11afdfc3c771fb9c2d653b))
- supervise Telegram bot + unify pairing-code store (closes #1499, closes #1500) ([#1505](https://github.com/bobmatnyc/trusty-tools/pull/1505)) ([`b5507a3`](https://github.com/bobmatnyc/trusty-tools/commit/b5507a3e59849f56e174af07c415ec448b4a7ee7))
- make telegram bot username configurable via TELEGRAM_BOT_USERNAME (default t_sess_bot) (refs #1433) ([#1497](https://github.com/bobmatnyc/trusty-tools/pull/1497)) ([`897df90`](https://github.com/bobmatnyc/trusty-tools/commit/897df90bdc25156aa8d190e7a4fdb0b5cd1a83c6))
- tmux lifecycle rollbacks — spawn send_line + registry upsert (#1456, #1457) ([#1468](https://github.com/bobmatnyc/trusty-tools/pull/1468)) ([`e471ee2`](https://github.com/bobmatnyc/trusty-tools/commit/e471ee2ead502b013f41ea59e40e75c13475ba45))
- tmux lifecycle — DELETE kills session + graceful-shutdown reaper (#1454, #1455) ([#1466](https://github.com/bobmatnyc/trusty-tools/pull/1466)) ([`4c53699`](https://github.com/bobmatnyc/trusty-tools/commit/4c5369923e592288e9b0e5d41dec028ec345078b))

### Changed

- WI-2 review nits — single missing-source hint + document deploy layout (refs #1548) ([#1556](https://github.com/bobmatnyc/trusty-tools/pull/1556)) ([`404b874`](https://github.com/bobmatnyc/trusty-tools/commit/404b8744e72ffeec07a2f1e12cb3eeeb217b38b1))
- rename CLI 'session' command group to 'sessions' (closes #1394) ([#1395](https://github.com/bobmatnyc/trusty-tools/pull/1395)) ([`864b006`](https://github.com/bobmatnyc/trusty-tools/commit/864b0062bfd0a3d913855c45fa9d246bca13634f))
- rename `tm coordinator-tui` → `tm session tui` + move coordinator API under /api/v1/sessions ([#1393](https://github.com/bobmatnyc/trusty-tools/pull/1393)) ([`749e7dd`](https://github.com/bobmatnyc/trusty-tools/commit/749e7dd4bad08ff8f93c04bb7c3d36991221f79b))

---

## [0.10.0] — 2026-06-17

### Fixed (closes #1373)

- **Sessions now register + pin their own project's trusty-search index.** At
  session launch `prepare_session` derives the project's canonical index id
  (git-root basename, via the shared `trusty_common::derive_index_id`),
  best-effort find-or-creates it in the running trusty-search daemon
  (`POST /indexes`), and injects the `trusty-search` MCP stub **pinned** to that
  id (`serve --index <id>`). A bare `search`/`grep` therefore resolves to the
  session's own project index instead of letting the LLM guess — which
  routinely picked the wrong (usually persistent `claude-mpm`) index. The
  daemon-unreachable case is graceful: it logs a warning and still pins the
  stub (the index is created on first reindex); an empty derived id falls back
  to the unpinned `serve` stub. Either way the session always launches.

## [0.9.0] — 2026-06-16

### Release

- **First monorepo publish.** This is the first `trusty-mpm` release published
  from the unified `trusty-tools` workspace. It supersedes the stale `0.8.1`
  on crates.io, which was published from the now-archived standalone repo.

### Fixed

- **Standalone build break:** `daemon/mcp_console.rs` imports
  `trusty_common::console_metrics` unconditionally, but that module is gated
  behind trusty-common's `console-metrics` feature. trusty-mpm's main
  `trusty-common` dependency now enables `console-metrics`, so
  `cargo check -p trusty-mpm` and `cargo publish` no longer fail to resolve
  the module. (Workspace feature-unification previously masked this under
  `cargo test`.)

## [0.8.2] — 2026-06-16

### Changed (closes part of #1318)

- **De-bundled `trusty-console`.** Removed the bundled `trusty-console`
  `[[bin]]` shim and dependency. `cargo install trusty-mpm` now produces
  `tm` and `trusty-mpm` only. Install the console with
  `cargo install trusty-console`. This is part of the single-owner-per-binary
  fix for the cargo binary-ownership collisions (#1262).

## [Unreleased]

### Changed: CLI command group `session` → `sessions` (issue #1394)

The top-level CLI command group was renamed from the singular `session` to the
plural **`sessions`** to match the `/api/v1/sessions/*` HTTP API surface. Every
subcommand is now invoked under the plural name, e.g. `tm sessions tui`,
`tm sessions ls`, `tm sessions new`.

- The singular `session` spelling is **removed entirely** — it is not retained
  as an alias. Invoking `tm session …` now fails with an
  unrecognized-subcommand error. Update any scripts or muscle memory to
  `tm sessions …`.
- This is a CLI-only change; the HTTP API (already `/api/v1/sessions/*`) and the
  separate `session-manager` / `sm` coordinator command are unaffected.

### Deprecated: verbose managed session-lifecycle verbs (issue #1205)

The managed session-lifecycle CLI verbs were renamed to the cleaner, symmetric
`stop` / `resume` / `decommission` family. The old verbose verbs still work but
now emit a one-line deprecation notice to **stderr** on every invocation and
will be removed in a future release.

| Deprecated verb | Use instead | Behavior |
|-----------------|-------------|----------|
| `tm sessions runtime-stop <id>` | `tm sessions stop <id>` | Stop the runtime, keep the workspace (resumable) |
| `tm sessions managed-stop <id>` | `tm sessions stop <id>` | Same as `runtime-stop` |
| `tm sessions managed-resume <id>` | `tm sessions resume <id>` | Re-spawn the runtime in the existing workspace |

- The deprecated verbs are hidden from `tm sessions --help` but continue to parse
  for backward compatibility.
- Each deprecated invocation prints `warning: '<old>' is deprecated; use '<new>'`
  to stderr; stdout stays clean for scripts.
- `tm sessions decommission <id>` (terminal teardown: remove workspace from disk)
  is unchanged.

## [0.5.0] — 2026-05-28

### Added: `tm services` — canonical service-discovery CLI (issue #339)

**New subcommand**: `tm services <action>` — replaces ad-hoc `lsof`/`curl`/`ps`
patterns for discovering the port, health, and status of every trusty-* daemon.

#### Subcommands

| Command | Description |
|---------|-------------|
| `tm services list [--json]` | Table of all declared services with running/down status, port, version, and health |
| `tm services status <name> [--json]` | Detailed block for one service |
| `tm services port <name>` | Print just the port number (scriptable) |
| `tm services url <name>` | Print the full base URL |
| `tm services health <name>` | Probe the `/health` endpoint; exit 0 if healthy |
| `tm services log <name>` | Print the log file path if it exists |
| `tm services init [--force]` | Write the default manifest to `~/.claude-mpm/services.yaml` |
| `tm services restart <name>` | Execute the manifest `restart_cmd` |

#### Manifest

Default manifest embedded in the binary covers 6 services:

- `trusty-search` — port 7878, `/health` confirmed
- `trusty-analyze` — port 7879, `/health` confirmed
- `trusty-mpm-daemon` — port 7880, `/health` confirmed at `daemon/api.rs:74`
- `trusty-memory` — dynamic port (7070-7079) via `~/.trusty-memory/http_addr`
- `trusty-embedderd` — UDS sidecar, pgrep-only (no HTTP surface)
- `trusty-bm25-daemon` — UDS sidecar, pgrep-only (no HTTP surface)

Custom manifests can be placed at `~/.claude-mpm/services.yaml` (use `tm services init`).

#### Exit codes

| Code | Meaning |
|------|---------|
| 0 | Running/healthy (list always exits 0) |
| 1 | Service declared but down, or health probe failed |
| 2 | Service name not in manifest |

#### Scriptable usage

```bash
PORT=$(tm services port trusty-search)
URL=$(tm services url trusty-search)
tail -f $(tm services log trusty-search)
```

#### Architecture

- `crates/trusty-mpm/src/services/manifest.rs` — `ServicesManifest`, `ServiceDecl`,
  `PortDiscovery` enum, `ManifestValidationError` (thiserror)
- `crates/trusty-mpm/src/services/discoverer.rs` — `Discoverer` with 5-second
  TTL cache; `ProcessProber`/`PortProber`/`HttpProber`/`VersionRunner` trait
  seams for unit testing
- `crates/trusty-mpm/assets/default-services.yaml` — embedded default manifest

**Tests**: 21 new unit tests (8 manifest + 13 discoverer, all mocked) + 11 CLI
parse tests + 2 ignore-gated integration smoke tests.

---

## [consolidation] — 2026-05-26

**Combined 7 trusty-mpm-\* sub-crates into one crate with feature-gated `[[bin]]` targets.**

### Summary

The following sub-crates have been merged into this unified `trusty-mpm` crate:

| Former crate | Now lives in |
|---|---|
| `trusty-mpm-core` | `crates/trusty-mpm/src/core/` |
| `trusty-mpm-client` | `crates/trusty-mpm/src/client/` |
| `trusty-mpm-mcp` | `crates/trusty-mpm/src/mcp/` (feature: `mcp`) |
| `trusty-mpm-daemon` | `crates/trusty-mpm/src/daemon/` (feature: `daemon`) |
| `trusty-mpm-cli` | `crates/trusty-mpm/src/bin/tm.rs` (feature: `cli`) |
| `trusty-mpm-tui` | `crates/trusty-mpm/src/tui/` (feature: `tui`) |
| `trusty-mpm-telegram` | `crates/trusty-mpm/src/telegram/` (feature: `telegram`) |

The Tauri desktop GUI (`trusty-mpm-gui`) remains as a separate crate because
it owns `build.rs` (invoking `tauri_build::build()`) and `tauri.conf.json` — files
that cannot co-exist with a generic Cargo crate build system. The `gui` feature of
this crate wraps it as an optional path dependency.

### Workspace crate count
- Removed: 7 crates (`trusty-mpm-core`, `trusty-mpm-mcp`, `trusty-mpm-daemon`,
  `trusty-mpm-client`, `trusty-mpm-cli`, `trusty-mpm-tui`, `trusty-mpm-telegram`)
- Added: 1 crate (`trusty-mpm`)
- Net change: 28 → 22 workspace members

### Feature flags

| Feature | What it enables |
|---|---|
| `default` | `cli` + `daemon` (the common install path) |
| `cli` | `tm` / `trusty-mpm` CLI binary (implies `daemon`, `tui`, `telegram`) |
| `daemon` | `trusty-mpmd` daemon binary + daemon library module (implies `mcp`) |
| `mcp` | MCP server library module |
| `tui` | `trusty-mpm-tui` shim binary + TUI library module |
| `telegram` | `trusty-mpm-telegram` shim binary + Telegram library module |
| `gui` | `trusty-mpm-gui` shim binary (wraps the separate `trusty-mpm-gui` crate) |

### Public API surface

All public types, traits, and functions are preserved. The only change is the
import path: code that previously imported from `trusty_mpm_core`, `trusty_mpm_client`,
etc. should now import from the corresponding submodule of `trusty_mpm`:

```rust
// Before
use trusty_mpm_core::session::{Session, SessionId};
use trusty_mpm_client::DaemonClient;

// After
use trusty_mpm::core::session::{Session, SessionId};
use trusty_mpm::client::DaemonClient;
```

### Deprecation notes

The following crate names are no longer published:
- `trusty-mpm-core`
- `trusty-mpm-mcp`
- `trusty-mpm-daemon`
- `trusty-mpm-client`
- `trusty-mpm-cli`
- `trusty-mpm-tui`
- `trusty-mpm-telegram`

All functionality is available under `trusty-mpm` with the appropriate feature flags.

## [0.4.0] and prior

See the individual crate changelogs in the former sub-crate directories (available
in git history as `crates/trusty-mpm-{core,client,mcp,daemon,cli,tui,telegram}/`).
