#!/usr/bin/env bash
#
# live_check_11786_unlock_selftest.sh — offline checks of the #11786 live
# check's pure helpers.
#
# Why: a live check that misjudges ancestry or miscounts prompts reports a
#   false PASS on the owner's Mac, where nobody watches it run.
# What: sources live_check_11786_unlock.sh in source-only mode and feeds its
#   helpers fixtures: ancestry verdicts (native, npm and non-Claude chains),
#   log classification of the three marker shapes, window counting, `list`
#   redaction to counts, audit counting, and `--dry-run` output. Touches no
#   daemon, no tmux and no `log`.
# Test: this file is the test; run it directly. Exit 0 when every case passes.

set -u
HERE=$(cd "$(dirname "$0")" && pwd)
# shellcheck disable=SC2034 # read by the sourced script
LIVE_CHECK_11786_SOURCE_ONLY=1
# shellcheck source=/dev/null
. "$HERE/live_check_11786_unlock.sh"

T=$(mktemp -d "${TMPDIR:-/tmp}/lc11786-selftest.XXXXXX")
trap 'rm -rf "$T"' EXIT
fails=0
check() { # check <name> <expected> <actual>
  if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1: expected [$2] got [$3]"; fails=$((fails + 1)); fi
}
tab=$(printf '\t')

# --- ancestry
printf '10%s9%s/bin/sh%ssh probe.sh\n9%s1%s/opt/homebrew/bin/tmux%stmux -L x new-session\n' \
  "$tab" "$tab" "$tab" "$tab" "$tab" "$tab" > "$T/clean"
judge_ancestry "$T/clean" > /dev/null; check ancestry_clean_tmux_chain 0 $?

printf '10%s9%s/bin/zsh%szsh\n9%s1%s/Users/u/.local/bin/claude%sclaude\n' \
  "$tab" "$tab" "$tab" "$tab" "$tab" "$tab" > "$T/native"
judge_ancestry "$T/native" > /dev/null; check ancestry_native_claude_is_agent 1 $?

printf '10%s1%s/Users/u/.local/share/claude/versions/2.1.9%s2.1.9\n' "$tab" "$tab" "$tab" > "$T/versioned"
judge_ancestry "$T/versioned" > /dev/null; check ancestry_versioned_path_is_agent 1 $?

printf '10%s1%s/usr/local/bin/node%snode /usr/lib/node_modules/@anthropic-ai/claude-code/cli.js\n' \
  "$tab" "$tab" "$tab" > "$T/npm"
judge_ancestry "$T/npm" > /dev/null; check ancestry_npm_claude_is_agent 1 $?

printf '10%s1%s/usr/local/bin/node%snode lint.js CLAUDE.md\n11%s1%s/w/.claude/worktrees/x/sh%ssh\n' \
  "$tab" "$tab" "$tab" "$tab" "$tab" "$tab" > "$T/lookalike"
judge_ancestry "$T/lookalike" > /dev/null; check ancestry_lookalikes_are_not_agents 0 $?

# --- log classification (shapes copied from `log show`, values elided)
cat > "$T/raw" <<'EOF'
Filtering the log data using "..."
Timestamp               Ty Process[PID:TID]
2026-11-24 21:39:33.036 Df coreauthd[55479:aa7855] [com.apple.LocalAuthentication:Server,Interactive] showing UI: <LACRemoteUIParams ID:388-345-0, SU: 0, Siri: 0, uid: 502, pid: 18693, lsai: 0>
2026-11-24 22:08:27.730 Df securityd[142:c9565a] [com.apple.securityd:kcacl] displaying keychain prompt for /Users/u/.cargo/bin/trusty-secrets(5758); ACL: <...>
2026-11-24 22:08:27.965 Df SecurityAgent[5968:c9adfb] [com.apple.Authorization:SecurityAgent] SC confirmation dialog detected
2026-11-24 22:09:00.000 Df securityd[142:d2f] [com.apple.securityd:kcacl] suppressing keychain prompt /Users/u/.cargo/bin/trusty-secrets(5758); code signing check failed rc=-67065
2026-11-24 22:10:00.000 Df securityd[142:d2f] [com.apple.securityd:kcacl] displaying keychain prompt for /Applications/Other.app/Contents/MacOS/Other(77); ACL: <...>
EOF
classify_log "$T/raw" > "$T/ev"
check classify_line_count 5 "$(wc -l < "$T/ev" | tr -d ' ')"
check classify_la_pid "18693" "$(awk -F'\t' '$2=="LA_PROMPT"{print $3}' "$T/ev")"
check classify_kc_pid "5758" "$(awk -F'\t' '$2=="KC_PROMPT"{print $3}' "$T/ev")"
check classify_suppressed 1 "$(grep -c KC_SUPPRESSED "$T/ev")"
check classify_other_app 1 "$(grep -c KC_OTHER "$T/ev")"
check classify_keeps_no_message 0 "$(grep -c 'ACL\|trusty-secrets' "$T/ev")"

# --- window counting: [start, end)
check window_inside 1 "$(count_in_window "$T/ev" "2026-11-24 22:08:00.000" "2026-11-24 22:09:00.000" KC_PROMPT)"
check window_end_exclusive 0 "$(count_in_window "$T/ev" "2026-11-24 22:09:00.000" "2026-11-24 22:09:59.000" KC_PROMPT)"
check window_pid_filter 1 "$(count_in_window "$T/ev" "2026-11-24 21:00:00.000" "2026-11-24 23:00:00.000" LA_PROMPT 18693)"
check window_pid_mismatch 0 "$(count_in_window "$T/ev" "2026-11-24 21:00:00.000" "2026-11-24 23:00:00.000" LA_PROMPT 1)"

# --- list redaction
printf 'trusty/o/r (Project):\n  API_KEY\n  DB_PASSWORD (push-only for agents)\ntrusty/o (Owner):\n  (no keys)\n' > "$T/list"
check redact_list_counts "vaults=2 keys=2" "$(redact_list "$T/list")"

# --- audit counting
cat > "$T/audit" <<'EOF'
{"ts":100,"stream":"credential_access","method":"secrets.unlock","decision":"allow","agent_parent":false}
{"ts":200,"stream":"credential_access","method":"secrets.lock","decision":"allow"}
{"ts":300,"stream":"credential_access","method":"secrets.unlock","decision":"deny","agent_parent":true}
{"ts":400,"stream":"credential_access","method":"secrets.unlock","decision":"allow","agent_parent":true}
EOF
check audit_allows_since 1 "$(audit_count 150 secrets.unlock allow "$T/audit" "$T/missing")"
check audit_allows_all 2 "$(audit_count 0 secrets.unlock allow "$T/audit")"
check audit_lock_any_decision 1 "$(audit_count 0 secrets.lock "" "$T/audit")"
check audit_unlock_drop_not_unlock 0 "$(printf '{"ts":500,"method":"secrets.unlock.drop","decision":"allow"}\n' | audit_count 0 secrets.unlock allow /dev/stdin)"
check audit_agent_parent 2 "$(audit_unlock_agent_parent 0 "$T/audit")"

# --- dry run executes nothing and names the predicate
out=$(TM_BIN=/nonexistent/tm "$HERE/live_check_11786_unlock.sh" --dry-run 2>&1); rc=$?
check dry_run_exit 0 "$rc"
check dry_run_banner 1 "$(printf '%s\n' "$out" | grep -c '^DRY RUN')"
check dry_run_predicate 1 "$(printf '%s\n' "$out" | grep -c "^PLAN: log stream .*showing UI")"
check dry_run_deny_asserts 6 "$(printf '%s\n' "$out" | grep -c '^ASSERT D-')"
out=$(TM_BIN=/nonexistent/tm "$HERE/live_check_11786_unlock.sh" --dry-run --approve 2>&1)
check dry_run_approve_asserts 5 "$(printf '%s\n' "$out" | grep -c '^ASSERT A-')"

[ "$fails" = 0 ] && { echo "selftest: all passed"; exit 0; }
echo "selftest: $fails failed"; exit 1
