#!/usr/bin/env bash
# cutover.sh - one-off move of the Architect from OLD to NEW (the owner's cutover ruling 12).
#
# Why: the Architect ran from OLD (the old supervisor checkout). NEW=architect is a verified
# clone. The tmux session name `tm-supervisor` stays, because fleet-poll.py
# (SUPERVISOR), its watchdog and every PM target that name.
# What: Phase A (old Architect alive; any failure aborts, nothing changed beyond
# the poller/verify sessions): A1 preconditions, A2 stop poller + verify session,
# A3 wait idle then kill tm-supervisor. Phase B (old Architect dead; a failure
# notifies, logs rollback commands, exits; OLD is never relaunched automatically):
# B1 sync local/, B2 inbox move + symlink, B3 config backup + banner removal +
# `tm fleet init`, B5 first prompt (after an RC restore), B6 poller from NEW, B7 summary.
# Run it detached from the OLD session:
#   nohup scripts/cutover.sh --yes >/dev/null 2>&1 &
# It ignores SIGHUP so killing the OLD pane does not stop it.
# Usage: scripts/cutover.sh [--dry-run] [--yes]   (a real run needs --yes)
# Test: `scripts/cutover.sh --dry-run` changes nothing (tests/test_cutover_dryrun.sh).
#
# What `tm fleet init` does (trusty-tools, the trusty-mpm `tm fleet` command source):
#  - mod.rs:239-345 init_with_session. Order: preflight, parse both configs, refuse a
#    second Architect, create dir, `git init` only if dir/.git is absent (mod.rs:411;
#    never adds a remote; an existing repo is left alone), write profile=supervisor to
#    <dir>/.trusty-mpm.toml, append dir to [supervisor] projects, seed files, launch,
#    start poller, then register with the daemon.
#  - mod.rs:401-409 refuse_second_architect: refuses while another allow-listed dir
#    with profile=supervisor exists (OLD has one), or while tmux session
#    <session> already runs in another dir. Hence B3 first removes OLD from projects.
#  - session: `--session` is recorded as [supervisor] session only when it differs from
#    the recorded value (session_name.rs:131-137); here it is already tm-supervisor, so
#    no write. The poller name is <session>-poll (tm-supervisor-poll).
#  - preflight.rs:119-141 refuses home, a workspace parent, a path inside a git work
#    tree, a repo with a remote, a nested repo except <dir>/local. NEW passes.
#  - seed.rs:84-113 writes seed files ONLY when absent; edited files are skipped, so
#    NEW's own scripts/CLAUDE.md are kept. It adds the local/ skeleton and .claude/skills/.
#  - launch.rs:147-215 starts tmux session <session> on the `opus` alias, records the
#    claude PID as the bound Architect. Failure after create kills the session.
#  - poller.rs:57-113: init itself starts NEW/scripts/start-fleet-poll.sh with NO
#    --interval (default 60 s). B3 stops that poller at once (no wake typed before the
#    first prompt); B6 restarts it with --interval 600.
#  - register.rs:55-82: daemon unreachable is only a warning (Skipped).
#  - init exits 1 if any step is FAILED; the session may already be running.
set -euo pipefail
trap '' HUP

OLD="${CUTOVER_OLD:-/path/to/old-checkout}"   # the old checkout (#8436: no operator path)
NEW="$(cd "$(dirname "$0")/.." && pwd)"
SESSION=tm-supervisor
POLL=tm-supervisor-poll
CFG="$HOME/.trusty-mpm/config.toml"
LOG="$NEW/inbox/cutover.log"
START_EPOCH=$(date +%s)
STAMP=$(date -u +%Y%m%dT%H%M%SZ)
DRY=0; YES=0
CUR=init
CONFIG_BAK=""; INBOX_MOVED=""

for a in "$@"; do
  case "$a" in
    --dry-run) DRY=1 ;;
    --yes) YES=1 ;;
    *) echo "usage: $0 [--dry-run] [--yes]" >&2; exit 2 ;;
  esac
done
if [ "$DRY" = 0 ] && [ "$YES" = 0 ]; then
  echo "refusing: a real run needs --yes (or use --dry-run)" >&2; exit 2
fi

log() {
  local line; line="$(date -u +%Y-%m-%dT%H:%M:%SZ) [$CUR] $*"
  if [ "$DRY" = 1 ]; then echo "DRY $line"; else
    mkdir -p "$NEW/inbox"; echo "$line" >> "$LOG"; echo "$line"
  fi
}
# act: run a mutating command, or only print it under --dry-run.
act() {
  if [ "$DRY" = 1 ]; then log "would run: $*"; return 0; fi
  log "run: $*"; "$@"
}
notify() {
  if [ "$DRY" = 1 ]; then log "would notify: $1"; return 0; fi
  osascript -e "display notification \"$1\" with title \"Architect cutover\"" >/dev/null 2>&1 || true
}

rollback_log() {
  log "ROLLBACK commands (run by hand, in this order; OLD is not relaunched automatically):"
  log "  tmux kill-session -t '=$SESSION'   # only if a half-started new session exists"
  log "  tmux kill-session -t '=$POLL'      # only if the new poller runs"
  if [ -n "$CONFIG_BAK" ]; then
    log "  cp '$CONFIG_BAK' '$CFG'"
  fi
  if [ -n "$INBOX_MOVED" ]; then
    log "  rm '$OLD/inbox' && mv '$INBOX_MOVED' '$OLD/inbox'   # rm of a symlink, no -r"
  fi
  log "  cd '$OLD' && scripts/session-relaunch.sh $SESSION   # recreates the session, resumes"
  log "  cd '$OLD' && scripts/start-fleet-poll.sh --interval 600"
}
fail() {
  log "FAILED at $CUR: $*"
  notify "FAILED at $CUR"
  if [ "$DRY" = 0 ] && [ "${PHASE:-A}" = B ]; then rollback_log; else
    log "phase A failure: everything left running; nothing to roll back"
  fi
  exit 1
}
# soft: in a dry run, report a failed precondition and continue; else fail.
soft() { if [ "$DRY" = 1 ]; then log "WOULD ABORT: $*"; else fail "$*"; fi; }

PHASE=A

# ---------------------------------------------------------------- Phase A
CUR=A1
log "start: dry=$DRY old=$OLD new=$NEW"
DEFSOCK="/private/tmp/tmux-$(id -u)/default"
if [ -n "${TMUX:-}" ] && [ "${TMUX%%,*}" != "$DEFSOCK" ]; then
  soft "tmux socket ${TMUX%%,*} is not the default $DEFSOCK; tm launches on the default"
fi
unset TMUX TMUX_PANE TMUX_SOCKET || true

tmux has-session -t "=$SESSION" 2>/dev/null || soft "tmux session =$SESSION does not exist"
log "ok: session =$SESSION exists"

dirty=$(git -C "$NEW" status --porcelain | grep -v '^?? inbox/' || true)
if [ -n "$dirty" ]; then soft "NEW is not clean: $(printf '%s' "$dirty" | head -5 | tr '\n' '|')"
else log "ok: NEW git status clean (apart from inbox/)"; fi

REF=$(mktemp)
touch -t "$(date -r $((START_EPOCH - 900)) +%Y%m%d%H%M.%S)" "$REF"
SNAP=$(find "$OLD/.trusty-mpm/sessions" -name 'session-*.md' -newer "$REF" 2>/dev/null | sort | tail -1 || true)
rm -f "$REF"
if [ -z "$SNAP" ]; then
  soft "no snapshot under $OLD/.trusty-mpm/sessions newer than 15 min before start"
else
  log "ok: fresh snapshot $SNAP"
fi

CUR=A2
if tmux has-session -t "=$POLL" 2>/dev/null; then
  act tmux kill-session -t "=$POLL"
else log "poller =$POLL not running"; fi
if tmux has-session -t "=architect-verify" 2>/dev/null; then
  act tmux kill-session -t "=architect-verify"
else log "=architect-verify not running"; fi

CUR=A3
pane_busy() {
  tmux capture-pane -t "=$SESSION:" -p 2>/dev/null | grep -Eq 'esc to interrupt|[✻✽✶✳✢·] .*…'
}
if [ "$DRY" = 1 ]; then
  if pane_busy; then log "pane currently busy (dry run: one check only)"; else log "pane currently idle"; fi
  log "would wait for 3 idle checks 5 s apart (max 10 min), then kill =$SESSION"
else
  idle=0; waited=0
  while [ "$idle" -lt 3 ]; do
    if [ "$waited" -ge 600 ]; then fail "pane still busy after 10 min; nothing killed"; fi
    if pane_busy; then idle=0; else idle=$((idle + 1)); fi
    sleep 5; waited=$((waited + 5))
  done
  log "pane idle (3 checks); killing =$SESSION"
  tmux kill-session -t "=$SESSION"
  sleep 2
  if tmux has-session -t "=$SESSION" 2>/dev/null; then fail "=$SESSION still exists after kill"; fi
fi
PHASE=B

# ---------------------------------------------------------------- Phase B
ATTR1="🤖🤖🤖 Generated with trusty-mpm — https://github.com/example-org/trusty-tools"
ATTR2="Claude-Session: https://claude.ai/code/session_01Yau3wuBhWhHRTgAdiZnRYg"

CUR=B1
act rsync -a --exclude .git "$OLD/local/" "$NEW/local/"
if [ "$DRY" = 1 ]; then
  log "would: git -C NEW add local; commit if anything staged"
else
  git -C "$NEW" add local
  if git -C "$NEW" diff --cached --quiet; then log "local/ already in sync; no commit"
  else
    git -C "$NEW" commit -q -m "chore: sync local/ records from the old Architect at cutover" \
      -m "$ATTR1" -m "$ATTR2" || fail "git commit of local/ failed"
    log "committed local/ sync: $(git -C "$NEW" rev-parse --short HEAD)"
  fi
fi

CUR=B2
if [ "$DRY" = 1 ]; then
  n=$(find "$OLD/inbox" -maxdepth 1 -type f | wc -l | tr -d ' ')
  log "would copy $n regular files from $OLD/inbox to $NEW/inbox"
  log "would: ln -sfn ../local/briefs $NEW/inbox/briefs"
  log "would: mv $OLD/inbox $OLD/inbox.pre-cutover-$STAMP; ln -s $NEW/inbox $OLD/inbox"
else
  mkdir -p "$NEW/inbox"
  for f in "$OLD"/inbox/*; do
    [ -f "$f" ] && [ ! -L "$f" ] || continue
    cp -p "$f" "$NEW/inbox/" || fail "copy of $f failed"
  done
  log "copied inbox regular files"
  ln -sfn ../local/briefs "$NEW/inbox/briefs"
  [ -d "$NEW/inbox/briefs/" ] || fail "NEW/inbox/briefs does not resolve"
  INBOX_MOVED="$OLD/inbox.pre-cutover-$STAMP"
  mv "$OLD/inbox" "$INBOX_MOVED" || { INBOX_MOVED=""; fail "mv of OLD/inbox failed"; }
  ln -s "$NEW/inbox" "$OLD/inbox" || fail "ln -s of OLD/inbox failed"
  sample=$(find "$NEW/local/briefs/" -maxdepth 1 -type f -exec basename {} \; | head -1)
  [ -n "$sample" ] && [ -e "$OLD/inbox/briefs/$sample" ] \
    || fail "OLD/inbox/briefs/$sample does not resolve through the symlink"
  log "ok: OLD/inbox -> NEW/inbox; OLD/inbox/briefs/$sample resolves"
fi

CUR=B4
if [ "$DRY" = 1 ]; then
  log "would remove the VERIFICATION MODE banner from NEW/CLAUDE.md (commit 42d69eb) and commit"
else
  if head -1 "$NEW/CLAUDE.md" | grep -q '^# VERIFICATION MODE'; then
    if ! head -15 "$NEW/CLAUDE.md" | grep -q '^# Project Instructions'; then
      fail "banner end marker '# Project Instructions' not in the first 15 lines"
    fi
    tmp=$(mktemp)
    awk 'f{print;next} /^# Project Instructions/{f=1;print}' "$NEW/CLAUDE.md" > "$tmp"
    cat "$tmp" > "$NEW/CLAUDE.md"; rm -f "$tmp"
    git -C "$NEW" add CLAUDE.md
    git -C "$NEW" commit -q -m "chore: remove the verification-mode banner at cutover" \
      -m "$ATTR1" -m "$ATTR2" || fail "banner commit failed"
    log "banner removed: $(git -C "$NEW" rev-parse --short HEAD)"
  else
    log "no banner at the top of CLAUDE.md; nothing to remove"
  fi
fi

CUR=B3
EDIT_PY='
import sys, tomllib, os
cfg, old = sys.argv[1], sys.argv[2]
t = open(cfg, encoding="utf-8").read()
q = chr(34) + old + chr(34)
n = t.replace(q + ", ", "").replace(", " + q, "").replace(q, "")
assert n != t, "OLD not found in config"
d = tomllib.loads(n)
assert old not in d["supervisor"]["projects"]
assert d["supervisor"]["session"] == "tm-supervisor"
open(cfg + ".cutover-tmp", "w", encoding="utf-8").write(n)
os.replace(cfg + ".cutover-tmp", cfg)
'
if [ "$DRY" = 1 ]; then
  log "would: cp $CFG $CFG.bak-cutover-$STAMP"
  log "would: remove \"$OLD\" from [supervisor] projects (python3 + tomllib check)"
  log "would: tm fleet init --dir $NEW --session $SESSION; then stop the poller init started"
else
  CONFIG_BAK="$CFG.bak-cutover-$STAMP"
  cp -p "$CFG" "$CONFIG_BAK" || { CONFIG_BAK=""; fail "config backup failed"; }
  log "config backed up to $CONFIG_BAK"
  python3 -c "$EDIT_PY" "$CFG" "$OLD" || fail "config edit failed (config unchanged or restored from backup)"
  log "removed OLD from [supervisor] projects"
  if tm fleet init --dir "$NEW" --session "$SESSION" >> "$LOG" 2>&1; then
    log "tm fleet init exit 0"
  else
    fail "tm fleet init failed (output above in the log); session may be half-started"
  fi
  if tmux has-session -t "=$POLL" 2>/dev/null; then
    tmux kill-session -t "=$POLL"; log "stopped the 60 s poller init started (B6 restarts it at 600 s)"
  fi
  tmux has-session -t "=$SESSION" 2>/dev/null || fail "=$SESSION not running after init"
  if tm fleet status --dir "$NEW" --session "$SESSION" >> "$LOG" 2>&1; then log "tm fleet status: complete"
  else log "WARN tm fleet status exit non-zero (see log)"; fi
fi

CUR=B5
T="=$SESSION:"
IS="$NEW/scripts/input-state.py"
box_state() { python3 "$IS" "$T" | awk '{print $2}'; }
box_line() { python3 "$IS" "$T" | cut -d' ' -f2-; }
cap() { tmux capture-pane -t "$T" -p -J; }
squash() { tr -s '[:space:]' ' '; }
is_shell() {
  case "$(tmux display -p -t "$T" '#{pane_current_command}')" in
    bash|zsh|sh|fish|dash|ksh|-*) return 0 ;; *) return 1 ;;
  esac
}
FIRST_PROMPT_TAIL="and report to the owner."
if [ "$DRY" = 1 ]; then
  log "would wait (max 5 min) for an empty Claude input box; Enter on the standard folder-trust / .mcp.json dialogs only"
  log "would run $NEW/scripts/rc-restore.sh $SESSION BEFORE the first prompt (it skips a busy pane, so after is useless)"
  log "would send the cutover prompt: send-keys -l, verify box, Enter, verify submitted"
else
  ready=0
  for _ in $(seq 1 100); do
    sleep 3
    c=$(cap 2>/dev/null | squash || true)
    if printf '%s' "$c" | grep -Eq 'Do you trust the files in this folder|trust this folder'; then
      log "dialog: folder trust; accepting default (Enter)"; tmux send-keys -t "$T" Enter; continue
    fi
    if printf '%s' "$c" | grep -Eq 'New MCP server|MCP servers? found in .mcp.json|Use this and all future MCP servers'; then
      log "dialog: .mcp.json approval; accepting default (Enter)"; tmux send-keys -t "$T" Enter; continue
    fi
    is_shell && continue
    printf '%s' "$c" | grep -q 'esc to interrupt' && continue
    if [ "$(box_state)" = empty ]; then ready=1; break; fi
  done
  [ "$ready" = 1 ] || fail "no empty Claude input box within 5 min; pane tail: $(cap | grep -v '^\s*$' | tail -4 | tr '\n' '|')"
  log "input box empty"

  if [ -x "$NEW/scripts/rc-restore.sh" ]; then
    if "$NEW/scripts/rc-restore.sh" "$SESSION" >> "$LOG" 2>&1; then log "rc-restore: ok"
    else log "WARN rc-restore exit $? (non-fatal; see $NEW/inbox/relaunch.log)"; fi
    sleep 3
  fi
  [ "$(box_state)" = empty ] || fail "input box not empty after rc-restore: $(box_line)"

  TEXT="[cutover $(date -u +%H:%M)Z] You are the Architect, now running from $NEW after the owner's cutover ruling 12. Your instructions carry architect.md and AGENTS.md; read local/goals.md, the 'Cutover handoff' section of local/state.md and local/actions.md from 18:30Z. Then arm the inbox Monitor, confirm tm-supervisor-poll runs from this directory, check that the MCP servers load, and report to the owner."
  tmux send-keys -t "$T" -l "$TEXT"
  sleep 2
  st=$(box_state); line=$(box_line)
  typed=${line#typed | }
  case "$st" in typed) ;; *) fail "box state after typing is $st, expected typed" ;; esac
  [ "${#typed}" -ge 20 ] && [ "${TEXT#"$typed"}" != "$TEXT" ] \
    || fail "box does not start with the prompt text: $typed"
  cap | squash | grep -qF "$FIRST_PROMPT_TAIL" || fail "end of the prompt text not visible in the box"
  log "box holds the first prompt; pressing Enter"
  tmux send-keys -t "$T" Enter
  ok=0
  for _ in $(seq 1 15); do
    sleep 2
    c=$(cap | squash)
    if printf '%s' "$c" | grep -q 'esc to interrupt' || [ "$(box_state)" != typed ]; then ok=1; break; fi
  done
  [ "$ok" = 1 ] || fail "first prompt not acknowledged (box still holds it)"
  log "first prompt submitted and acknowledged"
fi

CUR=B6
if [ "$DRY" = 1 ]; then
  log "would run $NEW/scripts/start-fleet-poll.sh --interval 600 and verify cwd/script under $NEW"
else
  "$NEW/scripts/start-fleet-poll.sh" --interval 600 >> "$LOG" 2>&1 || fail "start-fleet-poll.sh failed"
  sleep 2
  tmux has-session -t "=$POLL" 2>/dev/null || fail "=$POLL not running after start"
  pcwd=$(tmux display -p -t "=$POLL:" '#{pane_current_path}')
  ppid=$(tmux display -p -t "=$POLL:" '#{pane_pid}')
  pcmd=$(ps -axo ppid=,pid=,command= | awk -v p="$ppid" '$1==p || $2==p' | grep -F "$NEW/scripts/fleet-poll.py" | head -1 || true)
  [ "$pcwd" = "$NEW" ] || fail "poller cwd is $pcwd, expected $NEW"
  [ -n "$pcmd" ] || fail "poller process does not run $NEW/scripts/fleet-poll.py"
  log "ok: poller cwd=$pcwd script under NEW"
fi

CUR=B7
log "SUMMARY: $([ "$DRY" = 1 ] && echo 'DRY RUN complete, nothing changed' || echo 'cutover complete'); elapsed $(( $(date +%s) - START_EPOCH )) s; config backup=${CONFIG_BAK:-none}; old inbox kept at ${INBOX_MOVED:-none}; OLD dir untouched apart from the inbox symlink"
notify "Architect cutover done"
