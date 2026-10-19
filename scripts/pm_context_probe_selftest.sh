#!/usr/bin/env bash
#
# pm_context_probe_selftest.sh — fixtures for the PM starting-context probe (#8998).
#
# Why: the probe's numbers are only as good as its stream-json readout and its
#   lever argv. A readout that sums the wrong usage fields, counts a repeated
#   message twice, or reads the call after ToolSearch instead of the one after
#   session_context_catchup reports a plausible wrong baseline. The child
#   environment must also never carry a credential variable.
# What: no `claude` and no network. Fixture streams drive `parse`; a fixture
#   results file drives `report`; `print-env` runs under a parent environment
#   holding token variables; the remaining pure functions run in-process.
#     parse-first-turn       input + cache creation + cache read of call 1
#     parse-dedup            a message id repeated across events counts once
#     parse-post-catchup     the call after the catchup result, not after ToolSearch
#     parse-catchup-error    FAIL, an is_error catchup result
#     parse-no-call          FAIL, quoting the result text (e.g. a login error)
#     report-table           medians, deltas, a failed lever, the inferred row
#     env-no-credentials     GH_TOKEN, CLAUDE_CODE_OAUTH_TOKEN, ANTHROPIC_API_KEY dropped
#     functions              argv per lever, settings bodies, tier names, refused roots
# Usage: ./scripts/pm_context_probe_selftest.sh
# Exit: 0 when every case matches; 1 otherwise, printing each mismatch.
# Test: this IS the test.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROBE="${REPO_ROOT}/scripts/pm_context_probe.py"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/pm-context-probe-selftest.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

FAILURES=0
CASES=0

# check <label> <want-exit> <pattern> <command...>: exit status and a fixed-string match.
check() {
  CASES=$((CASES + 1))
  local label="$1" want="$2" pattern="$3" out rc
  shift 3
  out="$("$@" 2>&1)"
  rc=$?
  if [ "$rc" != "$want" ]; then
    FAILURES=$((FAILURES + 1))
    echo "  FAIL: ${label}: expected exit ${want}, got ${rc}"
    printf '%s\n' "$out" | sed 's/^/        /'
  elif [ -n "$pattern" ] && ! printf '%s\n' "$out" | grep -qF -- "$pattern"; then
    FAILURES=$((FAILURES + 1))
    echo "  FAIL: ${label}: output does not contain '${pattern}'"
    printf '%s\n' "$out" | sed 's/^/        /'
  else
    printf '  ok   %-28s -> exit %s\n' "$label" "$rc"
  fi
}

# ------------------------------------------------------------------ fixtures
INIT='{"type":"system","subtype":"init","model":"m-1","tools":["Bash","Task"],"mcp_servers":[{"name":"trusty-mpm","status":"connected"}]}'
U1='{"input_tokens":2,"cache_creation_input_tokens":100,"cache_read_input_tokens":50,"output_tokens":9}'

cat >"$WORK/first.jsonl" <<EOF
$INIT
not json at all
{"type":"assistant","message":{"id":"m1","usage":$U1,"content":[{"type":"text","text":"OK"}]}}
{"type":"assistant","message":{"id":"m1","usage":$U1,"content":[{"type":"text","text":"more"}]}}
{"type":"assistant","message":{"id":"m2","usage":{"input_tokens":1,"cache_read_input_tokens":999},"content":[]}}
{"type":"result","subtype":"success","is_error":false,"result":"OK"}
EOF

cat >"$WORK/catchup.jsonl" <<EOF
$INIT
{"type":"assistant","message":{"id":"m1","usage":$U1,"content":[{"type":"tool_use","id":"t1","name":"ToolSearch","input":{}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"loaded"}]}}
{"type":"assistant","message":{"id":"m2","usage":{"input_tokens":2,"cache_read_input_tokens":200},"content":[{"type":"tool_use","id":"t2","name":"mcp__trusty-mpm__session_context_catchup","input":{}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"digest"}]}}
{"type":"assistant","message":{"id":"m3","usage":{"input_tokens":2,"cache_creation_input_tokens":5000,"cache_read_input_tokens":202},"content":[{"type":"text","text":"DONE"}]}}
{"type":"result","subtype":"success","is_error":false,"result":"DONE"}
EOF

sed 's/"content":"digest"}/"content":"daemon unreachable","is_error":true}/' \
  "$WORK/catchup.jsonl" >"$WORK/catchup-error.jsonl"

cat >"$WORK/no-call.jsonl" <<EOF
$INIT
{"type":"result","subtype":"success","is_error":true,"result":"Not logged in · Please run /login"}
EOF

cat >"$WORK/results.json" <<'EOF'
{"meta": {}, "runs": [
 {"lever": "baseline", "run": 1, "first_turn": 100, "leaked_pids": []},
 {"lever": "baseline", "run": 2, "first_turn": 104, "leaked_pids": []},
 {"lever": "baseline", "run": 3, "first_turn": 98, "leaked_pids": []},
 {"lever": "mcp-off", "run": 1, "first_turn": 60, "leaked_pids": []},
 {"lever": "skills-off", "run": 1, "error": "no assistant call with usage; result: boom", "leaked_pids": []},
 {"lever": "catchup", "run": 1, "first_turn": 101, "post_catchup": 151, "leaked_pids": []}
]}
EOF

# ------------------------------------------------------------------ cases
check parse-first-turn 0 '"first_turn": 152' python3 "$PROBE" parse "$WORK/first.jsonl"
check parse-dedup 0 '"calls": [152, 1000]' python3 "$PROBE" parse "$WORK/first.jsonl"
check parse-post-catchup 0 '"post_catchup": 5204' python3 "$PROBE" parse "$WORK/catchup.jsonl"
check parse-catchup-error 1 'session_context_catchup failed: daemon unreachable' \
  python3 "$PROBE" parse "$WORK/catchup-error.jsonl"
check parse-no-call 1 'result: Not logged in · Please run /login' \
  python3 "$PROBE" parse "$WORK/no-call.jsonl"
check report-baseline 0 '| baseline | 100 | 100, 104, 98 | +0 | measured |' \
  python3 "$PROBE" report "$WORK/results.json"
check report-delta 0 '| mcp-off | 60 | 60 | -40 | measured |' python3 "$PROBE" report "$WORK/results.json"
check report-failed 0 '| skills-off | - | 0/1 | - | failed: no assistant call' \
  python3 "$PROBE" report "$WORK/results.json"
check report-post-catchup 0 '| post-session_context_catchup | 151 | 151 | +51 | measured |' \
  python3 "$PROBE" report "$WORK/results.json"
check report-inferred 0 '| catchup payload (post - first turn, same run) | 50 | - | - | inferred |' \
  python3 "$PROBE" report "$WORK/results.json"

ENV_OUT="$(env GH_TOKEN=x CLAUDE_CODE_OAUTH_TOKEN=x ANTHROPIC_API_KEY=x TMUX=x \
  python3 "$PROBE" print-env --config-dir "$WORK" 2>&1)"
CASES=$((CASES + 1))
if printf '%s\n' "$ENV_OUT" | grep -qE 'TOKEN|API_KEY|^TMUX$' ||
  ! printf '%s\n' "$ENV_OUT" | grep -qx 'CLAUDE_CONFIG_DIR'; then
  FAILURES=$((FAILURES + 1))
  echo "  FAIL: env-no-credentials: child env names were:"
  printf '%s\n' "$ENV_OUT" | sed 's/^/        /'
else
  printf '  ok   %-28s -> exit 0\n' "env-no-credentials"
fi

mkdir -p "$WORK/cfg/skills/a" "$WORK/cfg/skills/b" "$WORK/cfg/agents"
printf -- '---\nname: alpha\ndescription: x\n---\nbody\n' >"$WORK/cfg/skills/a/SKILL.md"
printf 'no frontmatter\n' >"$WORK/cfg/skills/b/SKILL.md"
printf -- '---\nname: "ex-agent"\n---\n' >"$WORK/cfg/agents/x.md"
cat >"$WORK/functions.py" <<'EOF'
import sys
from pathlib import Path

sys.path.insert(0, sys.argv[1])
import pm_context_probe as p

work = Path(sys.argv[2])
fails = []

def expect(label, got, want):
    if got != want:
        fails.append(f"{label}: got {got!r}, want {want!r}")

argv = p.build_argv("baseline", Path("/pr"), Path("/mcp"), None, None, "hi", 1)
tail = argv[argv.index("--append-system-prompt-file"):]
expect("baseline argv order", tail, ["--append-system-prompt-file", "/pr", "--setting-sources",
       "user,project,local", "--mcp-config", "/mcp", "--disallowedTools", "Workflow,DesignSync",
       "--dangerously-skip-permissions"])
expect("no-prompt drops the prompt", "--append-system-prompt-file" in
       p.build_argv("no-prompt", Path("/pr"), Path("/mcp"), None, None, "hi", 1), False)
off = p.build_argv("mcp-off", Path("/pr"), Path("/mcp"), Path("/s"), "m", "hi", 1)
expect("mcp-off is strict with no file", ("--strict-mcp-config" in off, "--mcp-config" in off), (True, False))
expect("settings and model reach argv", (off[off.index("--settings") + 1], off[off.index("--model") + 1]), ("/s", "m"))

skills, agents = p.tier_names(work / "cfg")
expect("tier names", (skills, agents), (["alpha", "b"], ["ex-agent"]))
body = p.settings_body(("connectors", "skills", "agents"), skills, agents)
expect("settings body", body, {"disableClaudeAiConnectors": True,
       "skillOverrides": {"alpha": "off", "b": "off"},
       "permissions": {"deny": ["Agent(ex-agent)", "Task(ex-agent)"]}})
expect("no parts, no body", p.settings_body((), skills, agents), {})

for name in ("Documents", "Desktop"):
    try:
        p.check_root(Path.home() / name / "x")
        fails.append(f"check_root accepted ~/{name}")
    except p.ProbeError:
        pass
p.check_root(work)
try:
    p.child_env({"PATH": "/bin"}, work)
except p.ProbeError as exc:
    fails.append(f"child_env refused a clean env: {exc}")
p.ENV_ALLOWLIST = p.ENV_ALLOWLIST + ("GH_TOKEN",)
try:
    p.child_env({"GH_TOKEN": "x"}, work)
    fails.append("child_env let GH_TOKEN through a widened allowlist")
except p.ProbeError:
    pass

print("\n".join(fails) if fails else "functions ok")
sys.exit(1 if fails else 0)
EOF
check functions 0 'functions ok' python3 -I "$WORK/functions.py" "$REPO_ROOT/scripts" "$WORK"

echo
if [ "$FAILURES" -gt 0 ]; then
  echo "pm_context_probe_selftest: ${FAILURES} of ${CASES} case(s) FAILED"
  exit 1
fi
echo "pm_context_probe_selftest: all ${CASES} cases passed"
