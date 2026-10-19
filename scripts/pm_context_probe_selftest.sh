#!/usr/bin/env bash
#
# pm_context_probe_selftest.sh — fixtures for the PM starting-context probe (#8998).
#
# Why: the probe's numbers are only as good as its stream-json readout and its
#   lever argv. A readout that sums the wrong usage fields, counts a repeated
#   message twice, or reads the call after ToolSearch instead of the one after
#   session_context_catchup reports a plausible wrong baseline. The child
#   environment must also never carry a credential variable.
# What: no real `claude` and no network. Fixture streams drive `parse`; fixture
#   results files drive `report`; `print-env` runs under a parent environment
#   holding token variables; the pure functions run in-process; stub `claude`
#   and `tm` binaries drive `run` for the cleanup cases.
#     parse-first-turn       input + cache creation + cache read of call 1
#     parse-dedup            a message id repeated across events counts once
#     parse-post-catchup     the call after the catchup result, not after ToolSearch
#     parse-catchup-error    FAIL, an is_error catchup result
#     parse-no-call          FAIL, quoting the result text (e.g. a login error)
#     report-table           medians, deltas, a failed lever, the inferred row
#     fail-closed            FAIL rows for a non-zero exit, a result is_error, a
#                            first turn of 0, and a catchup run whose tool was
#                            never called; `parse` exits 1 on the first three
#     env-no-credentials     GH_TOKEN, CLAUDE_CODE_OAUTH_TOKEN, ANTHROPIC_API_KEY dropped
#     functions              argv per lever, settings bodies, tier names, refused roots
#     timeout-*              a stub `claude` that never answers: the group is
#                            reaped, the leader is not reported leaked
#     catchup-default-throwaway  the catchup lever reads the throwaway project
#                            unless --catchup-project opts in to a checkout
#     project-dir-*          --project-dir runs in that checkout (cwd, tm --dir),
#                            leaves it intact and clean, refuses a dirty one, and
#                            a project lever without it exits 2
#     signal-TERM/HUP        the probe killed mid-run exits 128+signal with no
#                            stub process and no temp tree left behind
# Usage: ./scripts/pm_context_probe_selftest.sh
# Exit: 0 when every case matches; 1 otherwise, printing each mismatch.
# Test: this IS the test.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROBE="${REPO_ROOT}/scripts/pm_context_probe.py"
# Physical path: a TMPDIR ending in `/` would otherwise leave a `//` that the
# probe's normalised paths never contain.
WORK="$(cd "$(mktemp -d "${TMPDIR:-/tmp}/pm-context-probe-selftest.XXXXXX")" && pwd -P)"
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
 {"lever": "catchup", "run": 1, "first_turn": 101, "post_catchup": 151, "leaked_pids": []},
 {"lever": "no-prompt", "run": 1, "first_turn": 90, "exit_code": 1, "leaked_pids": []},
 {"lever": "connectors-off", "run": 1, "first_turn": 95, "result_is_error": true, "leaked_pids": []},
 {"lever": "agents-off", "run": 1, "first_turn": 0, "leaked_pids": []}
]}
EOF

cat >"$WORK/results-no-catchup.json" <<'EOF'
{"meta": {}, "runs": [
 {"lever": "baseline", "run": 1, "first_turn": 100, "exit_code": 0, "leaked_pids": []},
 {"lever": "catchup", "run": 1, "first_turn": 101, "post_catchup": null, "exit_code": 0, "leaked_pids": []}
]}
EOF

cat >"$WORK/result-error.jsonl" <<EOF
$INIT
{"type":"assistant","message":{"id":"m1","usage":$U1,"content":[{"type":"text","text":"API Error: 529 overloaded"}]}}
{"type":"result","subtype":"success","is_error":true,"result":"API Error: 529 overloaded"}
EOF

cat >"$WORK/zero-usage.jsonl" <<EOF
$INIT
{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":0,"output_tokens":0},"content":[]}}
{"type":"result","subtype":"success","is_error":false,"result":""}
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
# #8998 review: a run that did not succeed is never measured.
check report-nonzero-exit 0 '| no-prompt | - | 0/1 | - | failed: claude exited 1' \
  python3 "$PROBE" report "$WORK/results.json"
check report-result-error 0 '| connectors-off | - | 0/1 | - | failed: result is_error' \
  python3 "$PROBE" report "$WORK/results.json"
check report-zero-first-turn 0 '| agents-off | - | 0/1 | - | failed: first turn is 0' \
  python3 "$PROBE" report "$WORK/results.json"
check report-catchup-not-called 0 '| catchup | - | 0/1 | - | failed: catchup tool not called' \
  python3 "$PROBE" report "$WORK/results-no-catchup.json"
check parse-result-error 1 'result is_error: API Error: 529 overloaded' \
  python3 "$PROBE" parse "$WORK/result-error.jsonl"
check parse-zero-first-turn 1 'first turn is 0' python3 "$PROBE" parse "$WORK/zero-usage.jsonl"

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

# --project-dir levers (#8998 condition 2): CLAUDE.md and project-tier skills.
proj = work / "proj"
(proj / ".claude" / "skills" / "ps").mkdir(parents=True)
(proj / ".claude" / "skills" / "ps" / "SKILL.md").write_text("---\nname: proj-skill\n---\n")
(proj / "CLAUDE.md").write_text("x\n")
(proj / "AGENTS.md").write_text("not read by claude\n")
info = p.project_tier(proj)
expect("project tier", info, {"claude_md": [str(proj / "CLAUDE.md")], "skills": ["proj-skill"],
       "agents": [], "mcp_servers": []})
expect("claude-md-off body", p.settings_body(("claude-md",), [], [], info),
       {"claudeMdExcludes": [str(proj / "CLAUDE.md")]})
expect("project-skills-off body", p.settings_body(("project-skills",), ["u"], [], info),
       {"skillOverrides": {"proj-skill": "off"}})
expect("skills + project-skills merge", p.settings_body(("skills", "project-skills"), ["u"], [], info),
       {"skillOverrides": {"u": "off", "proj-skill": "off"}})
expect("project levers are flagged", sorted(p.PROJECT_LEVERS),
       ["claude-md-off", "project-files-off", "project-skills-off"])
expect("every project lever exists", all(lv in p.LEVERS for lv in p.PROJECT_LEVERS), True)

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

# ------------------------------------------------------------------ cleanup
# A stub `claude` that never answers (it backgrounds a `sleep` and waits) and a
# stub `tm`; the probe's real `run` drives them. Each stub writes its pid.
make_stubs() {
  local dir="$1"
  mkdir -p "$dir/bin" "$dir/tmp"
  cat >"$dir/bin/claude" <<EOF
#!/usr/bin/env bash
[ "\${1:-}" = "--version" ] && { echo "stub-claude 0"; exit 0; }
printf '%s\n' "\$@" >"$dir/argv"
pwd -P >"$dir/cwd"
sleep 300 &
echo \$! >"$dir/sleep.pid"
echo \$\$ >"$dir/claude.pid"
wait
EOF
  cat >"$dir/bin/tm" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$@" >>"$(dirname "$0")/../tm.args"
case "$*" in *--version*) echo "stub-tm 0" ;; *instructions*) echo "STUB PM PROMPT" ;; *) exit 2 ;; esac
EOF
  chmod +x "$dir/bin/claude" "$dir/bin/tm"
}

stub_run() { # stub_run <dir> <timeout>: the probe against the stubs, in the foreground.
  PATH="$1/bin:$PATH" TMPDIR="$1/tmp" python3 "$PROBE" run --runs 1 --levers baseline \
    --tm "$1/bin/tm" --config-dir "$WORK/cfg" --timeout "$2"
}

# alive <dir>: the stub pids still running (empty = none).
alive() {
  local f
  for f in "$1/claude.pid" "$1/sleep.pid"; do
    [ -f "$f" ] && kill -0 "$(cat "$f")" 2>/dev/null && printf '%s ' "$(basename "$f" .pid)"
  done
}

reap_stubs() { local f; for f in "$WORK"/stub-*/*.pid; do [ -f "$f" ] && kill -9 "$(cat "$f")" 2>/dev/null; done; }
trap 'reap_stubs; rm -rf "$WORK"' EXIT

# timeout-reaps-leader: a run that times out kills its group, waits for the
# leader, and reports no leaked pid.
make_stubs "$WORK/stub-timeout"
check timeout-reaps-leader 1 '"leaked_pids": []' stub_run "$WORK/stub-timeout" 2
CASES=$((CASES + 1))
left="$(alive "$WORK/stub-timeout")$(ls -A "$WORK/stub-timeout/tmp")"
if [ -n "$left" ]; then
  FAILURES=$((FAILURES + 1))
  echo "  FAIL: timeout-cleans-up: left behind: $left"
else
  printf '  ok   %-28s -> nothing left\n' "timeout-cleans-up"
fi

# catchup-default-throwaway: with no --catchup-project the catchup prompt names
# the probe's own throwaway project, never this checkout (#8998 review).
make_stubs "$WORK/stub-catchup"
PATH="$WORK/stub-catchup/bin:$PATH" TMPDIR="$WORK/stub-catchup/tmp" python3 "$PROBE" run \
  --runs 0 --levers catchup --tm "$WORK/stub-catchup/bin/tm" --config-dir "$WORK/cfg" \
  --timeout 2 >/dev/null 2>&1
CASES=$((CASES + 1))
if grep -q "project_dir set to \"$WORK/stub-catchup/tmp/pm-context-probe-[^/]*/project\"" \
  "$WORK/stub-catchup/argv" && ! grep -qF "$REPO_ROOT" "$WORK/stub-catchup/argv"; then
  printf '  ok   %-28s -> throwaway project\n' "catchup-default-throwaway"
else
  FAILURES=$((FAILURES + 1))
  echo "  FAIL: catchup-default-throwaway: the catchup prompt was:"
  grep -F "project_dir" "$WORK/stub-catchup/argv" | sed 's/^/        /'
fi

# project-dir-*: --project-dir runs in that checkout, never initialises or
# removes it, and refuses a dirty one; a project lever needs it (#8998).
PROJ="$WORK/real-proj"
mkdir -p "$PROJ" && git init -q "$PROJ" && printf 'x\n' >"$PROJ/CLAUDE.md"
git -C "$PROJ" add CLAUDE.md && git -C "$PROJ" -c user.name=t -c user.email=t@t commit -qm init
PROJ="$(cd "$PROJ" && pwd -P)"
make_stubs "$WORK/stub-proj"
PATH="$WORK/stub-proj/bin:$PATH" TMPDIR="$WORK/stub-proj/tmp" python3 "$PROBE" run --runs 1 \
  --levers baseline --project-dir "$PROJ" --tm "$WORK/stub-proj/bin/tm" --config-dir "$WORK/cfg" \
  --timeout 2 >/dev/null 2>&1
CASES=$((CASES + 1))
if [ "$(cat "$WORK/stub-proj/cwd" 2>/dev/null)" = "$PROJ" ] && [ -f "$PROJ/CLAUDE.md" ] &&
  grep -qxF "$PROJ" "$WORK/stub-proj/tm.args" && [ -z "$(git -C "$PROJ" status --porcelain)" ] &&
  [ -z "$(ls -A "$WORK/stub-proj/tmp")" ]; then
  printf '  ok   %-28s -> ran in the checkout\n' "project-dir-used"
else
  FAILURES=$((FAILURES + 1))
  echo "  FAIL: project-dir-used: cwd=$(cat "$WORK/stub-proj/cwd" 2>/dev/null) tm args:"
  sed 's/^/        /' "$WORK/stub-proj/tm.args" 2>/dev/null
fi
printf 'dirty\n' >"$PROJ/untracked.txt"
check project-dir-dirty 2 'not clean' \
  env PATH="$WORK/stub-proj/bin:$PATH" TMPDIR="$WORK/stub-proj/tmp" python3 "$PROBE" run --runs 1 \
  --levers baseline --project-dir "$PROJ" --tm "$WORK/stub-proj/bin/tm" --config-dir "$WORK/cfg"
rm -f "$PROJ/untracked.txt"
check project-lever-needs-dir 2 'needs --project-dir' \
  env PATH="$WORK/stub-proj/bin:$PATH" TMPDIR="$WORK/stub-proj/tmp" python3 "$PROBE" run --runs 1 \
  --levers claude-md-off --tm "$WORK/stub-proj/bin/tm" --config-dir "$WORK/cfg"

# signal-<SIG>: the probe killed mid-run reaps the group, removes its temp
# tree and exits 128+signal.
for sig in TERM HUP; do
  dir="$WORK/stub-$sig"
  make_stubs "$dir"
  # `env` execs python3, so $! is the probe itself, not a subshell.
  env PATH="$dir/bin:$PATH" TMPDIR="$dir/tmp" python3 "$PROBE" run --runs 1 --levers baseline \
    --tm "$dir/bin/tm" --config-dir "$WORK/cfg" --timeout 120 >"$dir/out" 2>&1 &
  probe_pid=$!
  for _ in $(seq 1 100); do [ -s "$dir/claude.pid" ] && break; sleep 0.1; done
  kill "-$sig" "$probe_pid"
  wait "$probe_pid"
  rc=$?
  want=$((128 + $(kill -l "$sig")))
  CASES=$((CASES + 1))
  left="$(alive "$dir")$(ls -A "$dir/tmp")"
  if [ "$rc" != "$want" ] || [ -n "$left" ]; then
    FAILURES=$((FAILURES + 1))
    echo "  FAIL: signal-$sig: exit $rc (want $want); left behind: ${left:-nothing}"
    sed 's/^/        /' "$dir/out"
  else
    printf '  ok   %-28s -> exit %s, nothing left\n' "signal-$sig" "$rc"
  fi
done

echo
if [ "$FAILURES" -gt 0 ]; then
  echo "pm_context_probe_selftest: ${FAILURES} of ${CASES} case(s) FAILED"
  exit 1
fi
echo "pm_context_probe_selftest: all ${CASES} cases passed"
