# PM starting-context baseline (#8998)

The release-1 target is a PM first turn of 70,000 input tokens or less. This
file records the first-turn size of a throwaway PM, per lever, as measured by
`scripts/pm_context_probe.py`. Owner ruling df-D02 (a): this run on main is the
new baseline. The 10-01 numbers stay below as history.

## Baseline — 2026-10-19

| Lever | First-turn input tokens (median) | Runs | Delta vs baseline | Status |
|---|---|---|---|---|
| baseline | 49,972 | 49,972, 49,972, 49,972 | +0 | measured |
| no-prompt (appended PM prompt off) | 39,098 | 39,098, 39,098 | -10,874 | measured |
| mcp-off (every MCP server and connector off) | 37,398 | 37,398, 37,398 | -12,574 | measured |
| connectors-off (claude.ai connectors only) | 48,315 | 48,315, 48,315 | -1,657 | measured |
| skills-off | 46,221 | 46,221, 46,221 | -3,751 | measured |
| agents-off | 46,014 | 46,014, 46,014 | -3,958 | measured |
| skills-agents-off | 42,263 | 42,263, 42,263 | -7,709 | measured |
| all-trims (mcp, connectors, skills, agents off) | 29,689 | 29,689, 29,689 | -20,283 | measured |
| catchup run, first turn | 50,051 | 50,051, 50,051 | +79 | measured |
| post-`session_context_catchup` | 75,272 | 75,259, 75,286 | +25,300 | measured |
| catchup payload (post minus first turn, same run) | 25,222 | - | - | inferred |

"Tokens" is the first API call's `input_tokens + cache_creation_input_tokens +
cache_read_input_tokens`. Every lever gave the same number on every run.

### How it was run

```
cargo build -p trusty-mpm --bin tm          # main at dfe7ab34d3
./scripts/pm_context_probe.py run --runs 3 --lever-runs 2 \
    --tm "$CARGO_TARGET_DIR/debug/tm"
```

- tm 1.9.0 (main `dfe7ab34d3`), Claude Code 2.1.303, model `claude-opus-5-5`
  (the user tier's `model: opus`; a managed PM spawn passes no `--model`).
- PM prompt: 27,658 bytes, composed by `tm session instructions` for the
  throwaway project, which reads the installed content bundle.
- `CLAUDE_CONFIG_DIR=~/.trusty-tools/trusty-mpm/claude-config`: 59 user-tier
  skills, 44 agents, 9 user-scope MCP servers, 4 tm builtins and 1 claude.ai
  connector (`claude.ai Claude Docs`); 476 tools, of which 446 are MCP tools.
- Catchup project: the trusty-tools main checkout, the probe's default at
  `dbc5b743e0`. To reproduce the post-catchup row, pass
  `--catchup-project <trusty-tools main checkout>`. Since the review round the
  default is the throwaway project, because the `session_context_catchup`
  handler can fetch `refs/tm/sessions/**` and write the session cache
  (`crates/trusty-mpm/src/daemon/mcp_context.rs:222-226`,
  `crates/trusty-mpm/src/daemon/mcp_context_session_refs.rs:72-84`). That path
  needs a session id (`mcp_context_session_refs.rs:143`). The baseline's catchup
  call passed only `project_dir`, so for this run the handler returned at that
  line without writing.
- The run did not record claude's exit codes. Every run's result had
  `is_error: false`, a positive first turn and, for catchup, a post-catchup
  call, so the fail-closed check added in review leaves every row measured.

### Reading the rows

- **mcp-off** uses `--strict-mcp-config` with no `--mcp-config`. That drops
  the claude.ai connector too, so mcp-off includes connectors-off.
- **connectors-off**, **skills-off** and **agents-off** each pass a `--settings`
  file with the subtractive key the #8988/#8992 owner policy writes:
  `disableClaudeAiConnectors`, `skillOverrides` (every user-tier skill `off`),
  and `permissions.deny` (`Agent(<name>)`/`Task(<name>)` per user-tier agent).
- **baseline** carries no `--settings`. On main the carrier writes one only when
  an owner file applies, and a temp directory outside the repos root has no
  owner. A launch under an owner with a policy starts smaller than this row.
- The deltas overlap. skills-off plus agents-off alone is -7,709, and all four
  trims together are -20,283, not the sum of the single rows.

### Not measured

- **The real trusty-tools PM start.** The probe runs in a throwaway project, so
  it does not load the trusty-tools `CLAUDE.md`, its project-tier
  `.claude/` agents and skills, or the checkout's dev-override content. The
  10-01 runs put the real PM 15k-18k above the scratch PM; this probe does not
  re-measure that gap.
- **Interactive mode.** The probe uses `claude -p`; an interactive pane's
  first turn is not measured.
- **Leftovers outside the temp tree.** Claude Code records each throwaway cwd
  in `CLAUDE_CONFIG_DIR/.claude.json`. The probe does not edit that file,
  because it holds the login state. The probe's `tm hook` events also reach
  the tm daemon, because the user tier's hooks run as they do in a real PM.

## History — 10-01 (method and scripts lost)

| Measurement | First-turn input tokens |
|---|---|
| Scratch PM launch, 3 runs | 65,034 / 65,035 / 65,037 |
| Real trusty-tools PM, at start | 80.2k–83.3k |
| Real trusty-tools PM, after `session_context_catchup` | 94.7k–100.5k |

The 10-01 scripts (`probe208/claude-pm.py`) varied one lever at a time and
marked each source measured or inferred. Nothing records their launch flags
or Claude Code build, so the 65k-to-50k change between the two scratch
baselines does not isolate a cause.
