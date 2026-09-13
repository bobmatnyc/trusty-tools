# Token floor measurement (2026-09-13)

Controlled comparison of three levers against an engineer agent's per-turn
token floor: a BASE-AGENT prose rule, the #7683 `tools:` allowlist, and a
PreToolUse deny hook. Method: `claude -p --agent rust-engineer
--output-format stream-json` headless under OAuth login, cwd a scratch
project with the composed agent in `.claude/agents/`, target a clean
`origin/main` at `4243c20c6`, identical read-only prompt, two runs per arm.
Tokens are summed from each assistant record's `message.usage` (input +
cache_creation + cache_read); the first-turn sum is called the floor. The
prompt: "Explain how tm decides whether a project MCP server is trusted.
Read and cite the specific functions and files in crates/trusty-mpm involved
in that decision. This is a read-only investigation -- do not edit any
files. Keep the final answer under 250 words."

## Round 1: prose rule vs. allowlist

Three arms. A = rust-engineer composed at `e1813340d` (pre-#7683, no rule).
B = with the BASE-AGENT Read-over-Bash rule (`6aeac1fab`). C = rule plus the
#7683 `tools:` allowlist (tool schemas 580 to 28).

| Arm | Run | Turns | Floor | Total input | Bash readers | Read | Grep tool | Correct |
|---|---|---|---|---|---|---|---|---|
| A | 1 | 17 | 74,830 | 1,729,751 | 10 | 7 | 0 | yes |
| A | 2 | 11 | 74,252 | 978,639 | 2 | 4 | 0 | yes |
| B | 1 | 14 | 74,946 | 1,062,734 | 8 | 3 | 0 | yes |
| B | 2 | 16 | 73,115 | 1,425,206 | 11 | 4 | 0 | yes |
| C | 1 | 3 | 63,007 | 232,752 | 0 | 0 | 0 | yes |
| C | 2 | 6 | 63,007 | 522,168 | 0 | 2 | 0 | yes |

The prose rule did not change behavior: arm B's Bash-reader counts (8, 11)
sit inside arm A's range (2, 10), and B never used the native Grep tool
either. The "zero Bash readers" success criterion failed. The allowlist
(arm C) removed 11.5K from the floor — 15%, not the "few K" the #7683
estimate expected — and cut total input 3 to 5x, mainly through fewer turns
rather than a smaller floor per turn.

The 44.7K floor figure from the
[2026-09-12 spike report](input-token-optimization-spike-2026-09-12.md) is a
bare Haiku subagent dispatched with no CLAUDE.md, not an in-project engineer
session — it is not directly comparable to the 63-75K floors measured here.

## Round 2: floor decomposition

Re-measured arm C's floor at 62,612 and 63,007 (consistent with round 1).
Starting from arm C, each arm below removes one additional component,
cumulative, two runs each.

| Arm | Removed (cumulative) | Skills at init | Tools | Floor | Delta |
|---|---|---|---|---|---|
| C | none | 69 | 28 | 62,810 avg | |
| D1 | project CLAUDE.md (24,966 bytes) | 69 | 28 | 52,813 | 9,997 |
| D2 | aws-agents + aws-core plugins via project-tier `enabledPlugins: false` | 37 | 28 | 47,629 | 5,184 |
| D3 | composed agent body reduced to front matter + one line (56,767 to 621 bytes) | 37 | 28 | 27,648 | 19,981 |
| Residual | base prompt, 28 tool schemas, 37 builtin skill entries, 44-agent roster | 37 | 28 | 27,648 | |

The four deltas sum to 35,162, matching arm C's floor minus the residual.
The largest single component is the composed agent's own prose —
BASE-AGENT + BASE-ENGINEER + rust-engineer, stripped in D3 — at 19,981
tokens, 32% of arm C's floor. A rough bytes/4 estimate undershoots the
measured token cost by 30 to 40 percent for this markdown content, so do
not use it to predict a prose change's savings.

## Round 2: hook arm

A PreToolUse hook denying any Bash call that reads a repo file (`cat`,
`sed`, `head`, `tail`, `grep`, `rg`, `awk`, `less`, `more` against a path).

On arm C's agent (28-tool allowlist, no native Grep gap to route around)
the hook never fired: floor stayed 63,007, turns stayed 3 and 5.

On arm B's agent (BASE-AGENT rule, full tool schema, called BH here) the
hook changed behavior but not for the better. Bash read attempts fell from
8-11 to 3 and 1, every one denied, and the model fell back to
`mcp__trusty-search__grep` each time — never the native Grep tool. Turns
rose to 26 and 11 against a 14-16 baseline; total input came to 2,633,788
and 1,022,986, both higher than arm B's own baseline. The "no more than 2
extra turns" success criterion failed.

## Gotchas

- `--setting-sources project` also drops user-tier MCP servers and the
  agent roster (tools 28 to 7, agents 44 to 6) — it is not a skills-only
  lever, and a floor measured under it is not comparable to one without it.
- `--system-prompt-snapshot` does not write the prompt to a file; no flag in
  Claude Code 2.1.x exposes system-prompt bytes directly.
- Two checkouts of the same crate built into one shared `CARGO_TARGET_DIR`
  produced a silent stale-artifact false build — give each checkout its own
  target directory.
- Redirecting `CLAUDE_CONFIG_DIR` breaks OAuth login; do not use it to
  isolate a scratch project's config.

## Conclusions

The `tools:` allowlist (#7683) is the one lever in this measurement that
moved the floor and cut total input. The BASE-AGENT prose rule and the
PreToolUse deny hook both added nothing on top of it, and the hook made
arm B worse by adding turns without changing the agent's tool choice away
from Bash/MCP grep. The next measurable savings sit in agent prose (~20K,
round 2's D3) and project CLAUDE.md (~10K, round 2's D1) per turn — both
larger components than anything a behavioral prose rule can reach, because
they are paid once as fixed floor rather than contingent on model choice.

See also the [2026-09-12 spike report](input-token-optimization-spike-2026-09-12.md)
for the Haiku-subagent floor figures, the
[engineer transcript token-sinks report](engineer-transcript-token-sinks-2026-09-12.md)
for the original Bash-vs-Read observation this measurement tested, and
[epic #7681](https://github.com/bobmatnyc/trusty-tools/issues/7681) for the
tracking issue this work feeds.
