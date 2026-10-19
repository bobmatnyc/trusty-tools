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

- **A live trusty-tools PM.** The "Real trusty-tools PM" section measures the
  real project in a disposable checkout, not a running PM session.
- **Interactive mode.** The probe uses `claude -p`; an interactive pane's
  first turn is not measured.
- **Leftovers outside the temp tree.** Claude Code records each throwaway cwd
  in `CLAUDE_CONFIG_DIR/.claude.json`. The probe does not edit that file,
  because it holds the login state. The probe's `tm hook` events also reach
  the tm daemon, because the user tier's hooks run as they do in a real PM.

## Real trusty-tools PM — 2026-10-19

Closes the second #8998 condition: the same probe, pointed at this repo
(`--project-dir`), so the trusty-tools `CLAUDE.md`, its project-tier
`.claude/skills` and the tm-composed prompt for the checkout are loaded.

| Lever | First-turn input tokens (median) | Runs | Delta vs real baseline | Status |
|---|---|---|---|---|
| baseline | 55,731 | 55,731, 55,731, 55,731 | +0 | measured |
| no-prompt | 44,601 | 44,601, 44,601 | -11,130 | measured |
| mcp-off | 43,157 | 43,157, 43,157 | -12,574 | measured |
| connectors-off | 54,074 | 54,074, 54,074 | -1,657 | measured |
| skills-off (user tier) | 51,980 | 51,980, 51,980 | -3,751 | measured |
| agents-off (user tier) | 51,773 | 51,773, 51,773 | -3,958 | measured |
| skills-agents-off | 48,022 | 48,022, 48,022 | -7,709 | measured |
| all-trims | 35,448 | 35,448, 35,448 | -20,283 | measured |
| claude-md-off (project `CLAUDE.md`) | 50,568 | 50,568, 50,568 | -5,163 | measured |
| project-skills-off (`.claude/skills`, 2 skills) | 55,449 | 55,449, 55,449 | -282 | measured |
| project-files-off (both above) | 50,286 | 50,286, 50,286 | -5,445 | measured |
| project `.claude/agents` off | 55,731 | - | +0 | inferred: the directory does not exist |
| project `.mcp.json` off | 55,731 | - | +0 | inferred: the file does not exist |
| catchup run, first turn | 55,869 | 55,869, 55,869 | +138 | measured |
| post-`session_context_catchup` (disposable checkout) | 65,684 | 65,702, 65,667 | +9,953 | measured |
| catchup payload (post minus first turn, same run) | 9,816 | - | - | inferred |

Every lever gave the same number on every run. The agents and `.mcp.json` rows
are inferred from the probe's recorded inventory of the checkout
(`meta.project_tier`: one `CLAUDE.md`, skills `cargo-commands` and
`cargo-publish`, no agents, no MCP servers), not run.

### Gap to the empty-project baseline

Real baseline 55,731 minus empty baseline 49,972 = **5,759 tokens** (+11.5%).
The user tier is identical in both runs: mcp-off, connectors-off, skills-off,
agents-off, skills-agents-off and all-trims give the same deltas in both
tables, so none of the gap sits in user-tier content.

| Source | Tokens | How attributed | Status |
|---|---|---|---|
| Project `CLAUDE.md` (12,767 bytes) | +5,163 | real baseline minus claude-md-off; the empty project has none | measured |
| Composed PM prompt for this checkout (28,366 bytes vs 27,658) | +256 | real no-prompt delta (11,130) minus empty no-prompt delta (10,874) | measured |
| Project-tier skills (`cargo-commands`, `cargo-publish`) | +282 | real baseline minus project-skills-off | measured |
| Project `.claude/agents` | 0 | directory absent | inferred |
| Project `.mcp.json` | 0 | file absent | inferred |
| **Sum of attributed sources** | **+5,701** | | |
| Unattributed remainder | +58 | gap minus sum | unattributable by these levers |

`project-files-off` (-5,445) equals `claude-md-off` plus `project-skills-off`
(-5,163 and -282) exactly, so the two project levers are additive. The 58-token
remainder is whatever else differs between a git checkout with history and an
empty `git init`: the git-status block, the working-directory path and
`.trusty-mpm.toml`. No lever isolates those; the probe does not try.

### Against 10-01

The 10-01 real PM ran 15k-18k above its scratch PM. Today's gap is 5.8k, and
the first-turn total is 55.7k, below the 70k target. The 10-01 method is lost
(see History), so the 10-01 excess cannot be attributed to a source after the
fact.

### How it was run

```
git worktree add --detach <scratch>/real-checkout origin/main   # 6e5b9cb02e, clean
./scripts/pm_context_probe.py run --runs 3 --lever-runs 2 \
    --tm "$CARGO_TARGET_DIR/debug/tm" --project-dir <scratch>/real-checkout
```

- The project was a scratch worktree at `origin/main` (`6e5b9cb02e`), never the
  main checkout and never a live PM's directory. The probe refuses a dirty tree
  and re-checked it clean afterwards (`meta.project_clean_after: true`; the check ignores gitignored paths such as `.trusty-mpm/`). A
  scratch path has no ancestor `CLAUDE.md`, as with the main checkout.
- tm 1.9.0 debug build, Claude Code 2.1.303, model `claude-opus-5-5`, the same
  `CLAUDE_CONFIG_DIR` as the empty-project baseline. No stray processes remained.
- The catchup lever ran against that disposable checkout only (the
  `--project-dir` default), so it wrote no state in any real checkout.
- **claude-md-off** passes `claudeMdExcludes` with the project's `CLAUDE.md`
  path; **project-skills-off** passes `skillOverrides` for the project-tier
  skill names. The -5,163 and -282 moves show Claude Code 2.1.303 honours both.

### Catchup is not comparable to the main-checkout number

The post-catchup row above (65,684, payload 9,816) is lower than the
empty-project table's 75,272 (payload 25,222). That earlier call read the
trusty-tools main checkout; this one read a fresh worktree with no session
history. The ~15k difference is attributed to the main checkout's accumulated
session state. That is inferred from the two payloads, not measured: reproducing
it needs `--catchup-project` on the live checkout, and that handler can write
the session cache. The 10-01 real-PM catchup growth (+14.5k) stays unreproduced.

### Not measured here

- A running PM pane or interactive mode, and an owner-file `--settings` policy
  (the probe's baseline carries none).
- The git-status block and path contribution (inside the 58-token remainder).

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
