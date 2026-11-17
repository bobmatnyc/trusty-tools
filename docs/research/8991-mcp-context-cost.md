# Why each MCP server costs context, and what progressive-disclosure options exist (#8991)

Date: 2026-10-26. Epic: #8986. Status: research, point-in-time.

## Summary

- The four trusty-* servers in `.mcp.json` advertise 114 tools. Their `tools/list` payloads total 143,019 bytes, of which 82,507 are `inputSchema` JSON, 53,842 are tool descriptions and 1,588 are tool names.
- None of the measured servers sends `instructions` in `initialize` (0 bytes each). The ~10 KB of server instructions in the 2026-10-01 brief therefore comes from other servers (apex, heygen, notion, claude.ai connectors), not from trusty-*.
- Claude Code 2.1.310 defers MCP tool schemas by default (tool search). A deferred trusty-* server costs only its `mcp__<server>__<tool>` names at session start: 3,760 bytes for the four servers, 2.6% of the 143,019. No repo file or local environment turns tool search off (see Method, "Tool-search state").
- The full 143 KB is paid wherever tools load upfront. `docs/research/token-floor-measurement-2026-09-13.md:18-20` records 580 tool schemas in a headless `claude -p --agent` floor, so that path exists and is not covered by tool search in that measurement.
- Recommendation: shorten the descriptions of trusty-mpm's `session_context_catchup` and `session_context_pause` (see Recommendation).

## Method

Source: a `tools/list` capture, not an estimate. A Python driver (`capture.py`, kept in the session scratchpad, not committed) starts the installed binary over stdio, sends `initialize`, `notifications/initialized` and `tools/list`, and stores the raw replies. `analyze.py` then computes bytes.

Byte rules:

- All figures are UTF-8 bytes of compact JSON (`separators=(",", ":")`, no whitespace). Claude Code's own serialization may differ slightly; ratios between servers do not.
- `name B` = sum of tool `name` lengths. `desc B` = sum of `description` lengths. `schema B` = compact JSON of each `inputSchema`. `tools/list B` = compact JSON of the whole `tools` array. It exceeds the three parts by the JSON keys and punctuation (memory: 52,527 against 50,182).
- `upfront B` = sum of the `mcp__<server>__<tool>` names, the part Claude Code lists when a server is deferred.
- `instr B` = length of `result.instructions` in the `initialize` reply. It is 0 for every server below because the field is absent.

Capture environment:

- trusty-memory, trusty-review, slack-mcp, telegram-mcp, trusty-gworkspace-mcp: stdio binaries answer `tools/list` in-process (`crates/trusty-memory/src/commands/serve_stdio_local.rs:65`, `crates/trusty-review/src/mcp/mod.rs:150`). They ran under `env -i`-style minimal env with `HOME`, `TRUSTY_SANDBOX=1` and a scratch data dir. No daemon was involved.
- trusty-search: `serve` needs a daemon even for the handshake. I started one with `scripts/sandbox_search_daemon.sh` (`--dir` under `/tmp`), captured, then stopped it with the script's `--stop`. The capture is unpinned. A pinned session adds a ~280-byte note to each `index_id` property (`crates/trusty-search/src/mcp/tools/descriptors.rs:485`).
- trusty-mpm: `tm serve --stdio` forwards `tools/list` to the daemon (`crates/trusty-mpm/src/bin/tm/commands/serve_stdio.rs:339`). `scripts/sandbox_daemon.sh` refused to start with the installed `tm` 1.8.2: the daemon rejected `TMUX_TMPDIR` (installed binary older than the script). The bridge was therefore pointed at the already-running live daemon for one read-only `tools/list`. No daemon was started outside the launchers, and no cargo ran.

Installed versions: trusty-memory 0.33.0, trusty-search 0.61.0, trusty-review 0.38.3, trusty-mpm 1.8.2. The source at `main` is newer for trusty-mpm: `crates/trusty-mpm/src/mcp/tools/mod.rs:54` pins 39 tools, the capture returned 36. The three missing (`ask_owner`, `sessions_chat`, `dbatg`) were not measured; `sessions_chat` is hidden by the bridge unless `--allow-chat` (`serve_stdio.rs:339-352`).

Not measured: apex, duetto-code-intelligence, duetto-memory, duetto-product-intelligence (`.mcp.json:3-18`, authenticated HTTP servers), heygen, notion, the claude.ai connectors, trusty-kb (stdio start failed in the capture harness), trusty-analyze (its MCP wrapper needs a daemon). The issue asks for these too; they need an authenticated session and are not shipped by this repo.

Tool-search state: grep of `crates`, `scripts`, `docs`, project and user settings found no `ENABLE_TOOL_SEARCH`, `alwaysLoad`, `CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS` or `CLAUDE_CODE_MAX_MCP_DESCRIPTION_LENGTH`. `.mcp.json` sets no `alwaysLoad` on any server. Claude Code 2.1.310 is installed. Per the Claude Code MCP docs (https://code.claude.com/docs/en/mcp, "Scale with MCP tool search"), tool search is the default and only tool names and server instructions load at session start. I did not run `/context` to confirm the deferred state of a live session.

## Per-server measurements

Registered in `.mcp.json` (`.mcp.json:19-44`):

| Server | Tools | name B | desc B | schema B | tools/list B | upfront B (deferred) | instr B |
|---|---|---|---|---|---|---|---|
| trusty-memory | 53 | 746 | 20,059 | 29,377 | 52,527 | 1,806 | 0 |
| trusty-mpm (installed 1.8.2) | 36 | 572 | 22,562 | 16,658 | 41,397 | 1,184 | 0 |
| trusty-search | 21 | 222 | 9,761 | 25,942 | 36,880 | 642 | 0 |
| trusty-review | 4 | 48 | 1,460 | 10,530 | 12,215 | 128 | 0 |
| **Total (4 servers)** | **114** | **1,588** | **53,842** | **82,507** | **143,019** | **3,760** | **0** |

Shipped by the repo but not in `.mcp.json`:

| Server | Tools | name B | desc B | schema B | tools/list B | upfront B | instr B |
|---|---|---|---|---|---|---|---|
| trusty-gworkspace-mcp | 62 | 1,196 | 6,230 | 33,594 | 43,751 | 2,932 | 0 |
| slack-mcp | 22 | 425 | 5,094 | 7,859 | 14,351 | 777 | 0 |
| telegram-mcp | 3 | 59 | 666 | 669 | 1,527 | 116 | 0 |

Tool definitions live at:

- trusty-memory: `crates/trusty-memory/src/tools/definitions.rs:48,62` plus `chat_definitions.rs`, `room_definitions.rs`, `wing_definitions.rs`, `task_definitions.rs`, `backup_definitions.rs` and others in the same directory.
- trusty-mpm: `crates/trusty-mpm/src/mcp/tools/mod.rs:126` (`tool_catalog`), groups in `core.rs`, `session.rs`, `console.rs`, `project.rs`, `proxy.rs`, `disk.rs`, `secrets.rs`, `channels.rs`, `chat.rs`, `dbatg.rs`.
- trusty-search: `crates/trusty-search/src/mcp/tools/descriptors.rs:30`.
- trusty-review: `crates/trusty-review/src/mcp/tools.rs:67`.
- trusty-gworkspace-mcp: `crates/trusty-gworkspace/src/tools/mod.rs:31`. slack-mcp: `crates/trusty-channels/src/slack/tools.rs:108`. telegram-mcp: `crates/trusty-channels/src/telegram/tools.rs:84`.

## Where the bytes go

Largest tools (compact bytes, of which description):

- trusty-mpm: `session_context_catchup` 6,952 (4,219), `session_context_pause` 4,215 (2,541), `disk_survey` 3,555 (1,768), `config_write` 3,187 (1,420). Six tools carry a description over 1,000 bytes. The two `session_context_*` tools are 11,167 bytes, 27.0% of the server (`crates/trusty-mpm/src/mcp/tools/session.rs:320`). The catchup description is 4,219 bytes; Claude Code cuts an upfront tool description at 4,096 characters (MCP docs, "Tool search for MCP server authors").
- trusty-search: `search` 5,476 (1,655), `search_all` 4,389, `search_kg` 4,054, `search_lexical` 3,617, `grep` 3,581. The seven `search*` tools are 21,876 bytes, 59.3% of the server.
- trusty-memory: `memory_remember` 5,430 (733), `memory_recall` 3,321, `memory_note` 2,159. The palace (12), chat (10), room (3), wing (3) and task (3) tools are 31 tools and 23,656 bytes, 45.0% of the server.
- trusty-review: `review_pr` 6,667 and `review_diff` 4,828 are 94.1% of the server. Their schemas carry the same context-document properties twice.

Schema, not description, dominates three of the four servers (memory 56%, search 70%, review 86% of `tools/list B`). Property descriptions alone are 16,763 bytes in memory, 16,460 in search, 9,215 in mpm and 8,007 in review.

Identical property definitions repeated across tools (bytes recoverable if each distinct property were defined once; computed by `dup.py`):

| Server | Duplicate bytes | Share of tools/list B | Largest repeats |
|---|---|---|---|
| trusty-search | 10,139 | 27.5% | `project` x10 (3,096), `max_bytes` x5 (1,560), `index_id` x5 (1,036), `lexical_limit` x5 (1,004) |
| trusty-memory | 4,811 | 9.2% | `palace` (1,358 + 588), `attachments` x2 (896), `max_bytes` x3 (756) |
| trusty-review | 4,018 | 32.9% | `issue_docs`, `spec_docs`, `symbol_context`, `changed_files`, `claude_md`, each twice |
| trusty-mpm | 481 | 1.2% | `session_id` x6 (375) |
| trusty-gworkspace-mcp | 8,997 | 20.6% | `account` x58 (6,441) |

Issue references (`#NNNN`) appear 76 times in trusty-search descriptions and schemas, 52 in trusty-memory and 29 in trusty-mpm. They tell a model nothing it can act on.

## Options

"Exists" means present in the installed Claude Code 2.1.310 per the MCP docs. "Upstream" means a Claude Code or MCP-spec change. Savings use the measured bytes above; a saving marked "target" is a goal, not a measured result.

| # | Option | Exists / needs | Expected saving | Cost | Harness requirement |
|---|---|---|---|---|---|
| 1 | Tool search and deferral (default) | Exists, on by default; model must be Sonnet 4 / Opus 4 / Haiku 4.5 or later | 143,019 -> 3,760 bytes for the four registered servers (-139,259, 97.4%) | One `ToolSearch` round trip before the first call to a deferred tool. The upfront list still grows ~34 bytes per added tool. A model that searches poorly misses tools. | None. Must not be disabled by `ENABLE_TOOL_SEARCH=false`, an older model, or a gateway retry (MCP docs, "Configure tool search"). |
| 2 | Per-server `alwaysLoad` (true or false) | Exists, `false` needs v2.1.287+ | `false` keeps every tool of that server deferred, including tools the server marks `anthropic/alwaysLoad`; no bytes change for trusty-* today because none sets the mark. `true` on a hot server spends its full `tools/list B` for no search step. | `true` costs the server's whole payload every turn (search: 36,880) and makes startup wait up to 5 s. | Edit `.mcp.json` entries. |
| 3 | Per-server toggles and project-scoped server sets | Exists (`/mcp` panel, `disabledMcpServers`/`enabledMcpServers` in `~/.claude.json`, `--mcp-config`) | A whole server: review 12,215 when no review work is planned, gworkspace 43,751, slack 14,351 | The tools are unavailable until the server is enabled. A mid-session toggle changes the tool set. | tm would write the per-session server list when it spawns a session; no Claude Code change. |
| 4 | Lazy server start | Exists for remote servers with a `cached` entry (MCP docs); stdio servers connect in the background by default | Startup time and process count, not context | None for context. | None. |
| 5 | Per-session tool subsets (server-side profile flag) | Precedent in repo: `tm serve --stdio` hides `sessions_chat` unless `--allow-chat` (`serve_stdio.rs:339`); trusty-memory drops `palace` from `required` under `--palace` (`serve_stdio_local.rs:65`); trusty-search rewrites `index_id` when pinned (`descriptors.rs:485`). New work for a general profile. | memory without palace/chat/room/wing/task: -23,656 (45.0%). mpm without the 18 `session_*` tools: -23,321 (56.3%). Under deferral the saving is only the names of the removed tools (~34 bytes each). | A subset hides tools an agent may need; the profile name becomes a contract. Each server gets a flag and tests. | tm passes the flag in the generated `.mcp.json` per agent. No Claude Code change. |
| 6 | Agent `tools:` allowlists | Exists for tm agents: the #7683 allowlist cut tool schemas from 580 to 28 and the floor by 11.5K (`docs/research/token-floor-measurement-2026-09-13.md:18-20`). Whether it can name `mcp__server__tool` entries was not confirmed in the docs I read. | Up to the full upfront payload for agents that need no MCP tool | An agent without the tool cannot call it. | Already in the tm agent pipeline; confirm MCP-name patterns. |
| 7 | Consolidated meta-tools | New work | Dedup bound: search -10,139 (27.5%), review -4,018 (32.9%), memory -4,811 (9.2%), gworkspace `account` -6,441. Collapsing the seven `search*` tools into one `search` with a `lane` enum saves most of search's duplicate bytes (`search` already takes `mode`). | Breaking rename: every prompt, skill, doc and hook that names `mcp__trusty-search__search_lexical` and the like changes. One tool with an enum has a weaker description for tool search to match, and it hides rarely-used parameters. | None. Needs an ADR and a deprecation window; the old names can stay as hidden aliases. |
| 8 | Trim descriptions and schema prose | New work, no harness requirement | Target: `session_context_catchup` and `session_context_pause` descriptions from 6,760 to ~1,200 bytes = -5,560 (13.4% of trusty-mpm). Drop the 157 `#NNNN` references in trusty-* tool text. Property descriptions are 16,763 bytes (memory) and 16,460 (search). | Less in-tool guidance. Move the prose to the `tm-capabilities` skill reference (`tm generate capabilities` already renders `references/mcp-tools.md` from the catalog, `crates/trusty-mpm/src/bin/tm/generate/mcp_tools.rs:1-20`). | None. Claude Code already truncates upfront descriptions at 4,096 characters, so the tail of `session_context_catchup` is already cut. |
| 9 | Shared schema definitions (`$defs`/`$ref`) | Upstream: `$ref` inside one tool's `inputSchema` works per tool, not across tools; a cross-tool shared block needs MCP-spec or client support | Same bound as option 7 without renaming tools | Client behavior unknown; not tested. | Claude Code or MCP-spec change. |
| 10 | Server `instructions` as the search hook | Exists | Adds up to 4,096 characters per server upfront; pays back by letting tool search find the right tool | Costs bytes at session start for every session. trusty-* send none today. | None. |
| 11 | Dynamic `tools/list` with `list_changed` | Exists (Claude Code refetches on `list_changed`, MCP docs) | Same as option 5 if the server starts with a minimal set | Server must track session need; tool set changes mid-session. | None. |
| 12 | CLI or skill substitute for a rarely used server | Exists: tm already has CLI parity for several tools (`tm memory send-message`, `docs/adr/0019-unified-ipc-messaging-on-event-bus.md:27`) | A whole server, e.g. review 12,215 or gworkspace 43,751 | Loses the typed schema; one Bash call per use; the agent needs the skill text. | None. |
| 13 | Per-tool filtering at the client (allow/deny list inside one server) | Upstream. The docs I read list no such setting. | Option 5 without server flags | n/a | Claude Code change. |

## Recommendation

Shorten the descriptions of `session_context_catchup` and `session_context_pause` in trusty-mpm (`crates/trusty-mpm/src/mcp/tools/session.rs:320` and the adjacent `session_context_pause` entry) to about 600 bytes each, and move the removed prose into the `tm-capabilities` skill reference, which `tm generate capabilities` already builds from the same catalog.

- Applies to: trusty-mpm only.
- Measured basis: the two tools are 11,167 of 41,397 `tools/list` bytes (27.0%); their descriptions are 6,760 bytes. Cutting them to ~1,200 saves about 5,560 bytes (13.4% of the server) wherever its tools load upfront.
- Why this over the others: it is the single largest per-tool excess in the four servers, needs no harness change and no tool rename, and the 4,219-byte catchup description is already over the client's 4,096-character cut. Options 5 and 7 save more bytes on the upfront path but need a profile flag or a breaking rename and an ADR; they are the follow-up if the upfront path matters.
- Verification gate for the change: rerun `tools/list` and confirm the two description sizes. The rung is 6 (MCP schema surface) in the CLAUDE.md test ladder.
- Open question for #8986 before sub-issues 3 and 4 start: confirm with `/context` in a real tm-spawned session whether trusty-* tools show as `(deferred)`. If they do, the upfront cost of the four registered servers is 3,760 bytes and the remaining per-server bytes only matter on non-deferred paths.

## Docs rules for this file

- `docs/public-manifest.tsv:46-50` names `docs/research` a do-not-publish tree; the gate enforces it independently of the manifest (`scripts/check_public_docs.sh:194`). The file needs no manifest entry and cannot be published.
- `docs/reference/documentation-layout.md:19` and `:64` classify `research/` as dated historical evidence, "not silently rewritten to look current". This file carries a date and the installed versions for that reason.
- No script in `scripts/` or `.pre-commit-config.yaml` imposes a naming, frontmatter or length rule on `docs/research/` files. Existing names mix `<topic>-<date>.md` and `<issue>-<slug>.md` (`docs/research/6961-snapshot-consistency.md`); `8991-mcp-context-cost.md` follows the second form.
