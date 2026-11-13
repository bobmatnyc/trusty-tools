#!/usr/bin/env bash
#
# sandbox_daemon_selftest.sh — self-test for scripts/sandbox_daemon.sh
# (issue #9121).
#
# Why: the launcher's whole job is what it does NOT pass. A launcher that
#   quietly forwarded one inherited credential would look exactly like a
#   working one, so its allowlist and its refusals are tested before anyone
#   relies on it.
# What: runs the launcher against a stub `tm` that records the variable NAMES
#   and arguments it received, from a caller environment (built with `env -i`,
#   so the host's own variables cannot change the answer) polluted with fake
#   secrets. No real daemon starts. Cases:
#     allowlist     the stub sees exactly the five pinned names (HOME, PATH,
#                   TMUX_TMPDIR, TRUSTY_DATA_DIR_OVERRIDE, TRUSTY_SANDBOX;
#                   #6288 S3a dropped TRUSTY_MPM_ADDR, and the `--port` this
#                   case passes is ignored) plus the forwarded names the caller set
#                   (LANG, RUST_LOG), never the fake secrets or `LC_API_KEY`
#                   (plus the names /bin/sh adds itself), `daemon --sandbox`,
#                   and TMUX_TMPDIR=<dir>/data/tmux (#9557)
#     no-leak       no fake secret value appears in the launcher's output
#     daemon-socket the launcher prints <dir>/data/trusty-mpm/trusty-mpm.sock
#                   and the TRUSTY_MPM_SOCKET pin for it (#6288)
#     daemon-socket-too-long  a --dir whose daemon socket path is over
#                   sun_path refuses, even where the tmux path fits (#6288)
#     dry-run       prints the names, creates and starts nothing
#     subset        with every name the daemon allows set, the launcher passes
#                   only names on the daemon's closed allowlist
#                   (`ENV_ALLOWLIST` + `LOCALE_CATEGORIES` in daemon_sandbox.rs)
#     real-home     --dir naming the real home refuses (run as a dry run, so a
#                   regression prints instead of creating anything there)
#     symlink-home  --dir resolving to the real home through a symlink refuses
#     missing-dir   --dir that does not exist refuses
#     bad-bin       a --bin that is not executable refuses
#     bad-arg       an unknown argument is a usage error (exit 2)
#     cwd           run from a caller cwd beside a fake `.env.local`, with a
#                   relative --bin, the stub runs with cwd <dir>/home (#9161)
#     cwd-env-local a --dir below an ancestor `.env.local` refuses
#     cwd-symlinked-home  a <dir>/home symlinked into a tree under a
#                   `.env.local` refuses: the walk uses the physical path
#     cwd-home-env-local  <dir>/home/.env.local itself refuses
#     cwd-newline   a `.env.local` in an ancestor whose name ends in a newline
#                   refuses
#     cwd-newline-symlinked-home  <dir>/home linked to a newline-ended dir
#                   holding a `.env.local` refuses; the stub never runs
#     unresolvable-home  an existing <dir>/home that cannot be entered (mode
#                   000) refuses before the walk falls back to <dir>
#     cd-fails      under umask 0777 mkdir makes a home the launcher cannot
#                   enter; it exits 1 and the daemon never starts
#     teardown      after the daemon exits, the launcher runs
#                   `tmux -S <socket> kill-server` for `default` and a
#                   tm-ws-* socket beside it, and every tmux call addresses a
#                   sandbox socket by -S (#9557, #10922); skips without python3
#     teardown-ws   (#10922) a stub daemon starts a real tm-ws-* tmux server
#                   whose pane and its child ignore HUP and TERM; after
#                   teardown the server is gone, both processes are dead and
#                   the exit is 0; skips without tmux
#     teardown-survivor  (#10922) a stub daemon leaves a process outside tmux
#                   with its cwd under <dir>; the launcher names its pid and
#                   exits 1
#     teardown-symlink-entry  (#10922) a socket entry symlinked to a scratch
#                   decoy socket is skipped: no tmux call names it or its target
#     teardown-symlink-dir  (#10922) tmux-<uid> symlinked to a scratch decoy
#                   dir refuses the tmux step: no tmux call, exit 1
#     teardown-dead-pane  (#10922) a pane tmux reports dead is never signalled
#     teardown-socket-survivor  (#10922) a process with cwd / holding a bound
#                   unix socket under <dir> is named; exit 1
#     teardown-probe-failed  (#10922) a survivor probe that fails or is missing
#                   (SANDBOX_LSOF) is named on stderr; exit 1, never clean
#     bin-fresh-*   (#10755) against a temp git source root (SANDBOX_SRC_ROOT)
#                   and a stubbed dependency tree (BIN_FRESH_SOURCES_CMD): a
#                   stale build artifact refuses, in a dry run and in a real
#                   run that starts nothing, also via a symlink, a custom
#                   profile, a cargo-target-pool path, a newer dependency
#                   crate or an untracked module; the newest of several
#                   sources is named; a failed derivation or no source
#                   refuses; a fresh one and --allow-stale pass; an
#                   installed-style stub passes with a notice; the override
#                   is announced, and ignored with a warning unless
#                   BIN_FRESH_TEST=1
#     cred-*        (#10721) --credential NAME=<ref> against a perl stub that
#                   plays `tm secrets exec` and the daemon: the daemon sees
#                   exactly the pinned and forwarded names plus NAME, never a
#                   polluting secret, exported function or the exec's token
#                   fd name, and holds no fd above stderr (the stub opens a
#                   real token fd); a canary value is in no output, `ps` argv or file
#                   under the sandbox; a bad name, a malformed or empty ref,
#                   a repeat, and an unresolvable ref each refuse and start
#                   no daemon; without --credential a provider key in the
#                   caller env never arrives; the wrapper refuses a bad or
#                   unset NAME and a malformed or stdio token fd; and its FORWARDED list matches the launcher's
#   The two mode-000 cases skip as root, which enters any directory.
#
# Usage: ./scripts/sandbox_daemon_selftest.sh
# Exit:  0 when every case behaves; 1 naming each case that does not.
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI).

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAUNCHER="$SCRIPT_DIR/sandbox_daemon.sh"
PASSED=0
FAILED=0
# Physical, so it compares equal to the stub's `pwd -P` (macOS TMPDIR is a symlink).
# Under /tmp (#9557): the launcher refuses a tmux socket path over sun_path, and
# macOS's per-user TMPDIR plus a case dir is past it.
TMP_ROOT="$(cd "$(mktemp -d /tmp/tmsbx.XXXXXX)" && pwd -P)"
# chmod first: the mode-000 cases leave directories rm cannot descend into.
trap 'chmod -R u+rwx "$TMP_ROOT" 2>/dev/null; rm -rf "$TMP_ROOT"' EXIT

FAKE_TOKEN="9121-selftest-fake-token-value"
FAKE_KEY="9121-selftest-fake-key-value"
FAKE_LC="9121-selftest-fake-lc-value"
SANDBOX_RS="$SCRIPT_DIR/../crates/trusty-mpm/src/bin/tm/commands/daemon_sandbox.rs"

pass() { echo "ok   $1"; PASSED=$((PASSED + 1)); }
fail() { echo "FAIL $1: $2"; FAILED=$((FAILED + 1)); }

# The stub records names and arguments under the HOME it was given.
STUB="$TMP_ROOT/stub-tm"
cat > "$STUB" <<'STUB_EOF'
#!/bin/sh
env | cut -d= -f1 | sort > "$HOME/stub-env-names"
printf '%s\n' "$@" > "$HOME/stub-args"
pwd -P > "$HOME/stub-pwd"
printf '%s\n' "${TMUX_TMPDIR-unset}" > "$HOME/stub-tmux-tmpdir"
STUB_EOF
chmod +x "$STUB"

# The launcher, run from a cleared environment carrying fake secrets, two
# forwardable names, and an `LC_`-prefixed name that is no locale category.
# SRC_ROOT_ENV: extra env words, set by the bin-fresh cases only.
SRC_ROOT_ENV=()
run_launcher() {
  env -i PATH="$PATH" ${SRC_ROOT_ENV[@]+"${SRC_ROOT_ENV[@]}"} LANG=C RUST_LOG=warn LC_API_KEY="$FAKE_LC" \
    TELEGRAM_BOT_TOKEN="$FAKE_TOKEN" OPENAI_API_KEY="$FAKE_KEY" \
    SELFTEST_SECRET_TOKEN="$FAKE_TOKEN" bash "$LAUNCHER" "$@"
}

# daemon_allowlist: the names `tm daemon --sandbox` accepts, read from the
# two Rust constants. `DATA_DIR_OVERRIDE_ENV` is the one non-literal entry.
daemon_allowlist() {
  awk '/^const (ENV_ALLOWLIST|LOCALE_CATEGORIES)/ { on = 1; next }
       on && /^\];/ { on = 0 }
       on' "$SANDBOX_RS" \
    | sed -e 's/DATA_DIR_OVERRIDE_ENV/"TRUSTY_DATA_DIR_OVERRIDE"/' \
    | tr -d ' ",' | grep -E '^[A-Z_]+$' | sort -u
}

# launcher_names: every name the launcher's PINNED and FORWARDED lists carry.
launcher_names() {
  {
    grep '^PINNED="' "$LAUNCHER"
    sed -n '/^FORWARDED="/,/"$/p' "$LAUNCHER"
  } | sed -e 's/^[A-Z]*="//' | tr -d '\\"' | tr ' ' '\n' \
    | grep -E '^[A-Z_]+$' | sort -u
}

# real_home: the same password-database lookup the launcher uses.
real_home() {
  case "$(uname -s)" in
    Darwin) dscl . -read "/Users/$(id -un)" NFSHomeDirectory 2>/dev/null | awk '{print $2}' ;;
    *) getent passwd "$(id -u)" 2>/dev/null | cut -d: -f6 ;;
  esac
}

# 1. allowlist + no-leak: a real (stubbed) run.
DIR1="$TMP_ROOT/case1"
mkdir -p "$DIR1"
set +e
OUT1="$(run_launcher --bin "$STUB" --dir "$DIR1" --port 17999 2>&1)"
STATUS1=$?
set -e
if [ "$STATUS1" -ne 0 ]; then
  fail allowlist "launcher exit $STATUS1"
  printf '%s\n' "$OUT1" | sed 's/^/    /'
elif [ ! -f "$DIR1/home/stub-env-names" ]; then
  fail allowlist "the stub never ran"
else
  # /bin/sh exports PWD, SHLVL, OLDPWD and _ on its own; they are not inherited.
  GOT="$(grep -vxE 'PWD|OLDPWD|SHLVL|_' "$DIR1/home/stub-env-names" | tr '\n' ' ')"
  WANT="HOME LANG PATH RUST_LOG TMUX_TMPDIR TRUSTY_DATA_DIR_OVERRIDE TRUSTY_SANDBOX "
  if [ "$GOT" != "$WANT" ]; then
    fail allowlist "stub saw [$GOT], want [$WANT]"
  elif [ "$(tr '\n' ' ' < "$DIR1/home/stub-args")" != "daemon --sandbox " ]; then
    fail allowlist "stub args were not 'daemon --sandbox'"
  elif [ "$(cat "$DIR1/home/stub-tmux-tmpdir")" != "$DIR1/data/tmux" ]; then
    # #9557: the sandbox's own tmux socket directory, never the default.
    fail allowlist "TMUX_TMPDIR was not <dir>/data/tmux"
  else
    pass allowlist
  fi
fi
case "$OUT1" in
  *"$FAKE_TOKEN"*|*"$FAKE_KEY"*|*"$FAKE_LC"*) fail no-leak "a fake secret value reached the output" ;;
  *) pass no-leak ;;
esac
# #6288: the launcher names the daemon socket every tm client dials, and the
# TRUSTY_MPM_SOCKET pin that reaches it.
if printf '%s' "$OUT1" | grep -qxF "sandbox_daemon: daemon socket: $DIR1/data/trusty-mpm/trusty-mpm.sock" \
  && printf '%s' "$OUT1" | grep -qF "TRUSTY_MPM_SOCKET=$DIR1/data/trusty-mpm/trusty-mpm.sock"; then
  pass daemon-socket
else
  fail daemon-socket "the launcher did not print <dir>/data/trusty-mpm/trusty-mpm.sock"
fi

# 1b. daemon-socket-too-long (#6288): a --dir whose daemon socket path is over
# sun_path refuses before anything starts, even where the tmux path still fits.
LONG_PAD="$(printf 'x%.0s' $(seq 1 $((75 - ${#TMP_ROOT} - 1))))"
DIR1B="$TMP_ROOT/$LONG_PAD"
mkdir -p "$DIR1B"
set +e
OUT1B="$(run_launcher --bin "$STUB" --dir "$DIR1B" --dry-run 2>&1)"
STATUS1B=$?
set -e
if [ "$STATUS1B" -ne 1 ]; then
  fail daemon-socket-too-long "exit $STATUS1B (want 1) for a ${#DIR1B}-byte --dir"
elif ! printf '%s' "$OUT1B" | grep -qF "the daemon socket path is"; then
  fail daemon-socket-too-long "output lacks the daemon socket refusal: $OUT1B"
else
  pass daemon-socket-too-long
fi

# 2. dry-run: names printed, nothing created or started.
DIR2="$TMP_ROOT/case2"
mkdir -p "$DIR2"
set +e
OUT2="$(run_launcher --bin "$STUB" --dir "$DIR2" --dry-run 2>&1)"
STATUS2=$?
set -e
if [ "$STATUS2" -ne 0 ]; then
  fail dry-run "exit $STATUS2"
elif [ -e "$DIR2/home" ]; then
  fail dry-run "a dry run created $DIR2/home"
elif ! printf '%s' "$OUT2" \
    | grep -qF "names only): HOME PATH TRUSTY_DATA_DIR_OVERRIDE TRUSTY_SANDBOX TMUX_TMPDIR LANG RUST_LOG"; then
  fail dry-run "the passed names were not printed"
else
  case "$OUT2" in
    *"$FAKE_TOKEN"*|*"$FAKE_KEY"*|*"$FAKE_LC"*) fail dry-run "a fake secret value reached the output" ;;
    *) pass dry-run ;;
  esac
fi

# subset: with every name either side lists set, the launcher passes only
# names the daemon allows.
ALLOWED_NAMES="$(daemon_allowlist)"
CANDIDATES="$(printf '%s\n%s\n' "$ALLOWED_NAMES" "$(launcher_names)" | sort -u)"
if [ "$(printf '%s\n' "$ALLOWED_NAMES" | grep -c .)" -lt 10 ]; then
  fail subset "could not read the daemon allowlist from $SANDBOX_RS"
else
  ALL_SET=()
  for name in $CANDIDATES; do ALL_SET+=("$name=selftest"); done
  set +e
  OUT3="$(env -i "${ALL_SET[@]}" PATH="$PATH" bash "$LAUNCHER" \
    --bin "$STUB" --dir "$DIR2" --dry-run 2>&1)"
  STATUS3=$?
  set -e
  SUBSET_PASSED="$(printf '%s\n' "$OUT3" | sed -n 's/^sandbox_daemon: variables passed (names only): //p')"
  EXTRA_NAMES=""
  for name in $SUBSET_PASSED; do
    printf '%s\n' "$ALLOWED_NAMES" | grep -qx "$name" || EXTRA_NAMES="$EXTRA_NAMES $name"
  done
  if [ "$STATUS3" -ne 0 ] || [ -z "$SUBSET_PASSED" ]; then
    fail subset "launcher exit $STATUS3, passed [$SUBSET_PASSED]"
  elif [ -n "$EXTRA_NAMES" ]; then
    fail subset "the launcher passes names the daemon refuses:$EXTRA_NAMES"
  else
    pass subset
  fi
fi

# expect_refusal <name> <want-exit> <needle> <launcher args...>
expect_refusal() {
  local name="$1" want="$2" needle="$3" out status
  shift 3
  set +e
  out="$(run_launcher "$@" 2>&1)"
  status=$?
  set -e
  if [ "$status" -ne "$want" ]; then
    fail "$name" "exit $status, want $want"
    printf '%s\n' "$out" | sed 's/^/    /'
  elif ! printf '%s' "$out" | grep -qF -- "$needle"; then
    fail "$name" "output lacks '$needle'"
    printf '%s\n' "$out" | sed 's/^/    /'
  else
    pass "$name"
  fi
}

REAL="$(real_home)"
if [ -z "$REAL" ]; then
  fail real-home "the password database names no home; cannot run the case"
else
  # 3. Dry runs, so a regression prints instead of creating dirs in the real home.
  expect_refusal real-home 1 "resolves to the real home" --bin "$STUB" --dir "$REAL" --dry-run
  ln -s "$REAL" "$TMP_ROOT/home-link"
  expect_refusal symlink-home 1 "resolves to the real home" \
    --bin "$STUB" --dir "$TMP_ROOT/home-link" --dry-run
fi

expect_refusal missing-dir 1 "not an existing directory" \
  --bin "$STUB" --dir "$TMP_ROOT/does-not-exist" --dry-run
touch "$TMP_ROOT/not-executable"
expect_refusal bad-bin 1 "not an executable file" \
  --bin "$TMP_ROOT/not-executable" --dir "$DIR2" --dry-run
expect_refusal bad-arg 2 "unknown argument" --bin "$STUB" --no-such-flag

# 4. cwd (#9161): the caller sits beside a fake `.env.local` and names the stub
# by a relative path; the stub must run under <dir>/home, never that cwd.
LEAK="$TMP_ROOT/leak"
DIR4="$TMP_ROOT/case4"
mkdir -p "$LEAK" "$DIR4"
echo "OPENAI_API_KEY=$FAKE_KEY" > "$LEAK/.env.local"
set +e
OUT4="$(cd "$LEAK" && run_launcher --bin ../stub-tm --dir "$DIR4" 2>&1)"
STATUS4=$?
set -e
CWD4="$(cat "$DIR4/home/stub-pwd" 2>/dev/null || true)"
if [ "$STATUS4" -ne 0 ]; then
  fail cwd "launcher exit $STATUS4"
  printf '%s\n' "$OUT4" | sed 's/^/    /'
elif [ "$CWD4" != "$DIR4/home" ]; then
  fail cwd "stub cwd was [$CWD4], want [$DIR4/home]"
else
  pass cwd
fi
mkdir -p "$LEAK/sb"
expect_refusal cwd-env-local 1 "$LEAK/.env.local would be loaded" \
  --bin "$STUB" --dir "$LEAK/sb" --dry-run

# A <dir>/home symlinked into a tree under a `.env.local`: the daemon's cwd is
# the physical path, so the walk must follow the link.
LEAK2="$TMP_ROOT/leak2"
DIR5="$TMP_ROOT/case5"
mkdir -p "$LEAK2/real" "$DIR5"
touch "$LEAK2/.env.local"
ln -s "$LEAK2/real" "$DIR5/home"
expect_refusal cwd-symlinked-home 1 "$LEAK2/.env.local would be loaded" \
  --bin "$STUB" --dir "$DIR5" --dry-run
DIR6="$TMP_ROOT/case6"
mkdir -p "$DIR6/home"
touch "$DIR6/home/.env.local"
expect_refusal cwd-home-env-local 1 "$DIR6/home/.env.local would be loaded" \
  --bin "$STUB" --dir "$DIR6" --dry-run

# `$(dirname)` strips a trailing newline, so the walk would skip this ancestor.
NL_DIR="$TMP_ROOT/nl
"
mkdir -p "$NL_DIR/sb"
touch "$NL_DIR/.env.local"
expect_refusal cwd-newline 1 ".env.local would be loaded" \
  --bin "$STUB" --dir "$NL_DIR/sb" --dry-run

# <dir>/home links to a newline-ended dir; `$(pwd -P)` drops the newline, so
# the walk would start in another dir. Not a dry run: the stub must not start.
NL_HOME="$TMP_ROOT/nlhome
"
DIR9="$TMP_ROOT/case9"
mkdir -p "$NL_HOME" "$DIR9"
touch "$NL_HOME/.env.local"
ln -s "$NL_HOME" "$DIR9/home"
expect_refusal cwd-newline-symlinked-home 1 "cannot resolve <dir>/home" \
  --bin "$STUB" --dir "$DIR9"
[ ! -e "$NL_HOME/stub-pwd" ] || fail cwd-newline-symlinked-home "the stub ran"

if [ "$(id -u)" -eq 0 ]; then
  echo "skip unresolvable-home, cd-fails: root enters a mode-000 directory"
else
  # An existing home the launcher cannot resolve must refuse, not fall back
  # to walking from <dir>.
  DIR7="$TMP_ROOT/case7"
  mkdir -p "$DIR7/home"
  chmod 000 "$DIR7/home"
  expect_refusal unresolvable-home 1 "cannot resolve <dir>/home" \
    --bin "$STUB" --dir "$DIR7"
  chmod 700 "$DIR7/home"

  # The home is absent at the walk, and mkdir under umask 0777 creates it with
  # mode 000, so only the `cd` fails. The stub's marker is outside that home,
  # where a started daemon could still write it.
  DIR8="$TMP_ROOT/case8"
  mkdir -p "$DIR8"
  MARK_STUB="$TMP_ROOT/mark-stub"
  printf '#!/bin/sh\ntouch "%s"\n' "$DIR8/stub-ran" > "$MARK_STUB"
  chmod +x "$MARK_STUB"
  set +e
  OUT8="$(umask 0777 && run_launcher --bin "$MARK_STUB" --dir "$DIR8" 2>&1)"
  STATUS8=$?
  set -e
  chmod 700 "$DIR8/home" "$DIR8/data" 2>/dev/null || true
  if [ "$STATUS8" -ne 1 ] || [ -e "$DIR8/stub-ran" ]; then
    fail cd-fails "exit $STATUS8 (want 1), stub ran: $([ -e "$DIR8/stub-ran" ] && echo yes || echo no)"
    printf '%s\n' "$OUT8" | sed 's/^/    /'
  elif ! printf '%s' "$OUT8" | grep -qF "cannot enter <dir>/home"; then
    fail cd-fails "output lacks 'cannot enter <dir>/home'"
  else
    pass cd-fails
  fi
fi

# 10. teardown (#9557): after the daemon exits the launcher kills the server
# on the sandbox's own socket, by `-S` path, and touches no other server.
if command -v python3 >/dev/null 2>&1; then
  DIR10="$TMP_ROOT/case10"
  FAKE_BIN="$TMP_ROOT/fake-tmux-bin"
  mkdir -p "$DIR10/data/tmux/tmux-$(id -u)" "$FAKE_BIN"
  chmod 700 "$DIR10/data/tmux" "$DIR10/data/tmux/tmux-$(id -u)"
  SOCK10="$DIR10/data/tmux/tmux-$(id -u)/default"
  # #10922: a per-workspace server's socket beside `default`.
  WS10="$DIR10/data/tmux/tmux-$(id -u)/tm-ws-selftest-0123"
  for s in "$SOCK10" "$WS10"; do
    python3 -I -c 'import socket,sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' "$s"
  done
  # The fake answers no list-sessions, as a stopped server does.
  printf '#!/bin/sh\nprintf "%%s\\n" "$*" >> "%s"\ncase "$*" in *list-sessions*) exit 1 ;; esac\n' \
    "$TMP_ROOT/tmux-calls" > "$FAKE_BIN/tmux"
  chmod +x "$FAKE_BIN/tmux"
  set +e
  OUT10="$(env -i PATH="$FAKE_BIN:$PATH" bash "$LAUNCHER" --bin "$STUB" --dir "$DIR10" 2>&1)"
  STATUS10=$?
  set -e
  CALLS10="$(cat "$TMP_ROOT/tmux-calls" 2>/dev/null || true)"
  KILLS10="$(printf '%s\n' "$CALLS10" | grep 'kill-server' | tr '\n' ' ')"
  UNSCOPED10="$(printf '%s\n' "$CALLS10" | grep -v "^-S $DIR10/data/tmux/tmux-$(id -u)/" || true)"
  if [ "$STATUS10" -ne 0 ]; then
    fail teardown "launcher exit $STATUS10"
    printf '%s\n' "$OUT10" | sed 's/^/    /'
  elif [ "$KILLS10" != "-S $SOCK10 kill-server -S $WS10 kill-server " ]; then
    fail teardown "kill-server calls were [$KILLS10], want one per socket in the sandbox dir"
  elif [ -n "$UNSCOPED10" ]; then
    fail teardown "a tmux call did not address a sandbox socket by -S: [$UNSCOPED10]"
  elif ! printf '%s' "$OUT10" | grep -qF "tmux socket: $SOCK10"; then
    fail teardown "the launcher did not name the sandbox socket"
  else
    pass teardown
  fi
else
  echo "skip teardown: no python3 to bind a fake socket"
fi

# 10b. teardown-ws (#10922): a stub daemon starts a REAL per-workspace tmux
# server under the sandbox's TMUX_TMPDIR whose pane, and the pane's child,
# ignore HUP and TERM, as a `claude` outliving kill-server does; then it
# exits. Every tmux call here passes -S and runs with TMUX unset.
if command -v tmux >/dev/null 2>&1; then
  DIRWS="$TMP_ROOT/casews"
  mkdir -p "$DIRWS"
  WS_STUB="$TMP_ROOT/stub-ws-tm"
  cat > "$WS_STUB" <<'WS_EOF'
#!/bin/sh
# The launcher's env -i leaves TMUX unset; -L resolves under its TMUX_TMPDIR.
tmux -L tm-ws-selftest-4567 new-session -d -s ws \
  "sh -c 'trap \"\" HUP TERM; sleep 611 & echo \$! > \"$HOME/child-pid\"; echo \$\$ > \"$HOME/pane-pid\"; exec sleep 612'"
i=0
while [ "$i" -lt 50 ] && [ ! -s "$HOME/pane-pid" ]; do sleep 0.1; i=$((i + 1)); done
WS_EOF
  chmod +x "$WS_STUB"
  WSSOCK="$DIRWS/data/tmux/tmux-$(id -u)/tm-ws-selftest-4567"
  set +e
  OUTWS="$(env -i PATH="$PATH" bash "$LAUNCHER" --bin "$WS_STUB" --dir "$DIRWS" 2>&1)"
  STATUSWS=$?
  set -e
  PANE="$(cat "$DIRWS/home/pane-pid" 2>/dev/null || true)"
  CHILD="$(cat "$DIRWS/home/child-pid" 2>/dev/null || true)"
  if [ -z "$PANE" ] || [ -z "$CHILD" ]; then
    fail teardown-ws "the stub never started its tmux pane"
    printf '%s\n' "$OUTWS" | sed 's/^/    /'
  elif env -u TMUX tmux -S "$WSSOCK" list-sessions >/dev/null 2>&1; then
    fail teardown-ws "the tm-ws server still answers on $WSSOCK"
  elif kill -0 "$PANE" 2>/dev/null || kill -0 "$CHILD" 2>/dev/null; then
    fail teardown-ws "a pane process outlived teardown (pane $PANE, child $CHILD)"
  elif [ "$STATUSWS" -ne 0 ]; then
    fail teardown-ws "launcher exit $STATUSWS"
    printf '%s\n' "$OUTWS" | sed 's/^/    /'
  else
    pass teardown-ws
  fi
  # Whatever the result, leave nothing running: by -S path and by pid only.
  env -u TMUX tmux -S "$WSSOCK" kill-server >/dev/null 2>&1 || true
  for p in $PANE $CHILD; do kill -KILL "$p" 2>/dev/null || true; done
else
  echo "skip teardown-ws: no tmux"
fi

# 10c. teardown-survivor (#10922): a process outside tmux with its cwd under
# <dir> is not the launcher's to kill; teardown names it and exits 1.
DIRSV="$TMP_ROOT/casesv"
mkdir -p "$DIRSV"
SV_STUB="$TMP_ROOT/stub-sv-tm"
# shellcheck disable=SC2016  # expands in the stub, not here
printf '%s\n' '#!/bin/sh' 'sleep 613 > /dev/null 2>&1 &' 'echo $! > "$HOME/sv-pid"' > "$SV_STUB"
chmod +x "$SV_STUB"
set +e
OUTSV="$(env -i PATH="$PATH" bash "$LAUNCHER" --bin "$SV_STUB" --dir "$DIRSV" 2>&1)"
STATUSSV=$?
set -e
SVPID="$(cat "$DIRSV/home/sv-pid" 2>/dev/null || true)"
if [ -z "$SVPID" ]; then
  fail teardown-survivor "the stub never started its process"
elif [ "$STATUSSV" -ne 1 ] || ! printf '%s' "$OUTSV" | grep -q "teardown left processes under $DIRSV:.* $SVPID"; then
  fail teardown-survivor "exit $STATUSSV (want 1), survivor $SVPID not named"
  printf '%s\n' "$OUTSV" | sed 's/^/    /'
else
  pass teardown-survivor
fi
[ -z "$SVPID" ] || kill -KILL "$SVPID" 2>/dev/null || true

# 10d-10f use a fake tmux that logs every call and answers no list-sessions;
# $TMP_ROOT/fake-panes, when present, answers list-panes. The paths are baked
# in: the launcher runs tmux under `env -i`.
if command -v python3 >/dev/null 2>&1; then
  bind_sock() { python3 -I -c 'import socket,sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' "$1"; }
  LOGBIN="$TMP_ROOT/logtmux-bin"
  mkdir -p "$LOGBIN"
  printf '#!/bin/sh\nprintf "%%s\\n" "$*" >> "%s"\ncase "$*" in *list-sessions*) exit 1 ;; *list-panes*) cat "%s" 2>/dev/null ;; esac\nexit 0\n' \
    "$TMP_ROOT/calls" "$TMP_ROOT/fake-panes" > "$LOGBIN/tmux"
  chmod +x "$LOGBIN/tmux"
  # Scratch decoys only: a socket and a socket dir outside every case dir.
  DECOY_DIR="$TMP_ROOT/decoy/tmux-$(id -u)"
  mkdir -p "$DECOY_DIR"
  DECOY_SOCK="$DECOY_DIR/default"
  bind_sock "$DECOY_SOCK"

  # 10d. teardown-symlink-entry (#10922): a socket entry symlinked to the
  # decoy is skipped; `default` beside it is still stopped.
  DIRSE="$TMP_ROOT/casese"
  mkdir -p "$DIRSE/data/tmux/tmux-$(id -u)"
  chmod 700 "$DIRSE/data/tmux" "$DIRSE/data/tmux/tmux-$(id -u)"
  bind_sock "$DIRSE/data/tmux/tmux-$(id -u)/default"
  ln -s "$DECOY_SOCK" "$DIRSE/data/tmux/tmux-$(id -u)/tm-ws-decoy"
  rm -f "$TMP_ROOT/calls" "$TMP_ROOT/fake-panes"
  set +e
  OUTSE="$(env -i PATH="$LOGBIN:$PATH" bash "$LAUNCHER" --bin "$STUB" --dir "$DIRSE" 2>&1)"
  STATUSSE=$?
  set -e
  CALLSSE="$(cat "$TMP_ROOT/calls" 2>/dev/null || true)"
  if printf '%s\n' "$CALLSSE" | grep -qe "tm-ws-decoy" -e "$TMP_ROOT/decoy"; then
    fail teardown-symlink-entry "a tmux -S call named the symlinked entry or its decoy target: [$CALLSSE]"
  elif ! printf '%s\n' "$CALLSSE" | grep -qxF -- "-S $DIRSE/data/tmux/tmux-$(id -u)/default kill-server"; then
    fail teardown-symlink-entry "default was not stopped: [$CALLSSE]"
  elif [ "$STATUSSE" -ne 0 ]; then
    fail teardown-symlink-entry "launcher exit $STATUSSE"
    printf '%s\n' "$OUTSE" | sed 's/^/    /'
  else
    pass teardown-symlink-entry
  fi

  # 10e. teardown-symlink-dir (#10922): tmux-<uid> symlinked to the decoy dir
  # refuses the tmux step: no tmux call at all, exit 1, the refusal named.
  DIRSD="$TMP_ROOT/casesd"
  mkdir -p "$DIRSD/data/tmux"
  chmod 700 "$DIRSD/data/tmux"
  ln -s "$DECOY_DIR" "$DIRSD/data/tmux/tmux-$(id -u)"
  rm -f "$TMP_ROOT/calls" "$TMP_ROOT/fake-panes"
  set +e
  OUTSD="$(env -i PATH="$LOGBIN:$PATH" bash "$LAUNCHER" --bin "$STUB" --dir "$DIRSD" 2>&1)"
  STATUSSD=$?
  set -e
  CALLSSD="$(cat "$TMP_ROOT/calls" 2>/dev/null || true)"
  if [ -n "$CALLSSD" ]; then
    fail teardown-symlink-dir "tmux was called through the symlinked dir: [$CALLSSD]"
  elif [ "$STATUSSD" -ne 1 ] || ! printf '%s' "$OUTSD" | grep -q "teardown refused the tmux socket dir"; then
    fail teardown-symlink-dir "exit $STATUSSD (want 1), refusal not named"
    printf '%s\n' "$OUTSD" | sed 's/^/    /'
  else
    pass teardown-symlink-dir
  fi

  # 10f. teardown-dead-pane (#10922): a pane tmux reports dead keeps its pid
  # out of the TERM list, so a reused pid is never signalled.
  DIRDP="$TMP_ROOT/casedp"
  mkdir -p "$DIRDP/data/tmux/tmux-$(id -u)"
  chmod 700 "$DIRDP/data/tmux" "$DIRDP/data/tmux/tmux-$(id -u)"
  bind_sock "$DIRDP/data/tmux/tmux-$(id -u)/default"
  # Detached, so a stopped one is reaped by init, not left a zombie of ours.
  (sleep 614 > /dev/null 2>&1 & echo $! > "$TMP_ROOT/srv-dp")
  (sleep 615 > /dev/null 2>&1 & echo $! > "$TMP_ROOT/dead-dp")
  SRVDP="$(cat "$TMP_ROOT/srv-dp")"
  DEADDP="$(cat "$TMP_ROOT/dead-dp")"
  rm -f "$TMP_ROOT/calls"
  echo "$SRVDP $DEADDP 1" > "$TMP_ROOT/fake-panes"
  set +e
  OUTDP="$(env -i PATH="$LOGBIN:$PATH" bash "$LAUNCHER" --bin "$STUB" --dir "$DIRDP" 2>&1)"
  STATUSDP=$?
  set -e
  if ! kill -0 "$DEADDP" 2>/dev/null; then
    fail teardown-dead-pane "the dead pane's pid $DEADDP was signalled"
  elif kill -0 "$SRVDP" 2>/dev/null; then
    fail teardown-dead-pane "the server pid $SRVDP was not stopped"
  elif [ "$STATUSDP" -ne 0 ]; then
    fail teardown-dead-pane "launcher exit $STATUSDP"
    printf '%s\n' "$OUTDP" | sed 's/^/    /'
  else
    pass teardown-dead-pane
  fi
  kill -KILL "$SRVDP" "$DEADDP" 2>/dev/null || true

  # 10g. teardown-socket-survivor (#10922): a process with its cwd at / that
  # holds a bound unix socket under <dir> is a survivor; exit 1 names it.
  DIRSS="$TMP_ROOT/casess"
  mkdir -p "$DIRSS"
  SS_STUB="$TMP_ROOT/stub-ss-tm"
  # shellcheck disable=SC2016  # expands in the stub, not here
  printf '%s\n' '#!/bin/sh' 'cd / || exit 1' \
    'python3 -I -c "import socket,sys,time; s=socket.socket(socket.AF_UNIX); s.bind(sys.argv[1]); s.listen(1); time.sleep(616)" "$HOME/held.sock" > /dev/null 2>&1 &' \
    'echo $! > "$HOME/ss-pid"' \
    'i=0; while [ "$i" -lt 50 ] && [ ! -S "$HOME/held.sock" ]; do sleep 0.1; i=$((i + 1)); done' > "$SS_STUB"
  chmod +x "$SS_STUB"
  set +e
  OUTSS="$(env -i PATH="$PATH" bash "$LAUNCHER" --bin "$SS_STUB" --dir "$DIRSS" 2>&1)"
  STATUSSS=$?
  set -e
  SSPID="$(cat "$DIRSS/home/ss-pid" 2>/dev/null || true)"
  if [ -z "$SSPID" ] || [ ! -S "$DIRSS/home/held.sock" ]; then
    fail teardown-socket-survivor "the stub never bound its socket"
  elif [ "$STATUSSS" -ne 1 ] || ! printf '%s' "$OUTSS" | grep -q "teardown left processes under $DIRSS:.* $SSPID"; then
    fail teardown-socket-survivor "exit $STATUSSS (want 1), socket holder $SSPID not named"
    printf '%s\n' "$OUTSS" | sed 's/^/    /'
  else
    pass teardown-socket-survivor
  fi
  [ -z "$SSPID" ] || kill -KILL "$SSPID" 2>/dev/null || true
else
  echo "skip teardown-symlink-entry, teardown-symlink-dir, teardown-dead-pane, teardown-socket-survivor: no python3"
fi

# 10h. teardown-probe-failed (#10922): a survivor check that cannot run is a
# failure naming the check, never a clean teardown. SANDBOX_LSOF points the
# probe at a failing or a missing binary; a failing `lsof` also leads PATH.
PROBEBIN="$TMP_ROOT/probe-bin"
mkdir -p "$PROBEBIN"
printf '%s\n' '#!/bin/sh' 'exit 1' > "$PROBEBIN/lsof"
chmod +x "$PROBEBIN/lsof"
NPF=0
for probe in "$PROBEBIN/lsof:exited 1" "$TMP_ROOT/no-such-lsof:is missing or not executable"; do
  NPF=$((NPF + 1))
  DIRPF="$TMP_ROOT/casepf$NPF"
  mkdir -p "$DIRPF"
  set +e
  OUTPF="$(env -i PATH="$PROBEBIN:$PATH" SANDBOX_LSOF="${probe%%:*}" bash "$LAUNCHER" --bin "$STUB" --dir "$DIRPF" 2>&1)"
  STATUSPF=$?
  set -e
  if [ "$STATUSPF" -ne 1 ] || ! printf '%s' "$OUTPF" | grep -qF "survivor check failed: ${probe%%:*} ${probe#*:}"; then
    fail teardown-probe-failed "probe ${probe%%:*}: exit $STATUSPF (want 1), failed check not named"
    printf '%s\n' "$OUTPF" | sed 's/^/    /'
  else
    pass "teardown-probe-failed (${probe#*:})"
  fi
done

# 11. bin-fresh (#10755): a temp source root and a stubbed dependency tree
# (BIN_FRESH_SOURCES_CMD), so neither the real tree nor cargo changes the
# answer. Source mtimes: a.rs 2020-03, z.rs 2020-04, main.rs 2020-06 (the
# newest of several), trusty-secrets lib.rs 2020-08 (a dependency crate).
# Stubs: debug/tm and profiling/tm 2020-01 (stale), debug/tm-mid 2020-07
# (fresh against trusty-mpm alone, stale against its dependency), release/tm
# now (fresh).
SRC11="$TMP_ROOT/src11"
TGT11="$TMP_ROOT/tgt11"
mkdir -p "$SRC11/crates/trusty-mpm/src" "$SRC11/crates/trusty-secrets/src" \
  "$TGT11/debug" "$TGT11/release" "$TGT11/profiling"
for f in a z main; do echo "// $f" > "$SRC11/crates/trusty-mpm/src/$f.rs"; done
echo "// dep" > "$SRC11/crates/trusty-secrets/src/lib.rs"
touch -d 2020-03-01T00:00:00 "$SRC11/crates/trusty-mpm/src/a.rs"
touch -d 2020-04-01T00:00:00 "$SRC11/crates/trusty-mpm/src/z.rs"
touch -d 2020-06-01T00:00:00 "$SRC11/crates/trusty-mpm/src/main.rs"
touch -d 2020-08-01T00:00:00 "$SRC11/crates/trusty-secrets/src/lib.rs"
git -C "$SRC11" init -q
git -C "$SRC11" add crates
: > "$TGT11/CACHEDIR.TAG"
for b in debug/tm profiling/tm debug/tm-mid release/tm; do cp "$STUB" "$TGT11/$b"; done
touch -d 2020-01-01T00:00:00 "$TGT11/debug/tm" "$TGT11/profiling/tm"
touch -d 2020-07-01T00:00:00 "$TGT11/debug/tm-mid"
POOL11="$TMP_ROOT/cargo-target-pool/org/repo/slot-0/debug"
mkdir -p "$POOL11"
cp "$STUB" "$POOL11/tm"
touch -d 2020-01-01T00:00:00 "$POOL11/tm"
ln -s "$TGT11/debug/tm" "$TMP_ROOT/link-tm"
# The stubbed `cargo tree` output: trusty-mpm alone, or with its dependency.
TREE11="$TMP_ROOT/tree-tm"
# It runs in the source root, so it names crates under whichever root is set.
# shellcheck disable=SC2016  # $(pwd -P) expands in the stub, not here
printf '%s\n' '#!/bin/sh' 'echo "trusty-mpm v0.0.0 ($(pwd -P)/crates/trusty-mpm)"' \
  'echo "anyhow v1.0.0"' > "$TREE11"
# shellcheck disable=SC2016  # $(pwd -P) expands in the stub, not here
printf '%s\n' '#!/bin/sh' 'echo "trusty-mpm v0.0.0 ($(pwd -P)/crates/trusty-mpm)"' \
  'echo "trusty-secrets v0.0.0 ($(pwd -P)/crates/trusty-secrets) (*)"' > "$TREE11-deps"
chmod +x "$TREE11" "$TREE11-deps"
SRC_ROOT_ENV=("SANDBOX_SRC_ROOT=$SRC11" "BIN_FRESH_TEST=1" "BIN_FRESH_SOURCES_CMD=$TREE11")
expect_refusal bin-fresh-newest-of-several 1 \
  "is older than its newest source $SRC11/crates/trusty-mpm/src/main.rs" \
  --bin "$TGT11/debug/tm" --dir "$DIR2" --dry-run
DIR11="$TMP_ROOT/case11"
mkdir -p "$DIR11"
expect_refusal bin-fresh-stale 1 "refused: stale build artifact $TGT11/debug/tm" \
  --bin "$TGT11/debug/tm" --dir "$DIR11"
[ ! -e "$DIR11/home" ] || fail bin-fresh-stale "the launcher created <dir>/home"
expect_refusal bin-fresh-symlink 1 "refused: stale build artifact $TGT11/debug/tm" \
  --bin "$TMP_ROOT/link-tm" --dir "$DIR2" --dry-run
expect_refusal bin-fresh-profile 1 "refused: stale build artifact $TGT11/profiling/tm" \
  --bin "$TGT11/profiling/tm" --dir "$DIR2" --dry-run
expect_refusal bin-fresh-pool 1 "refused: stale build artifact $POOL11/tm" \
  --bin "$POOL11/tm" --dir "$DIR2" --dry-run
expect_refusal bin-fresh-fresh 0 "fresh: build artifact" \
  --bin "$TGT11/release/tm" --dir "$DIR2" --dry-run
expect_refusal bin-fresh-allow-stale 0 "warning: --allow-stale" \
  --bin "$TGT11/debug/tm" --dir "$DIR2" --dry-run --allow-stale
expect_refusal bin-fresh-installed 0 "notice: installed binary (not under a cargo target dir)" \
  --bin "$STUB" --dir "$DIR2" --dry-run
# An untracked new module counts (2020-07-15 > tm-mid's 2020-07-01).
echo "// new" > "$SRC11/crates/trusty-mpm/src/new_mod.rs"
touch -d 2020-07-15T00:00:00 "$SRC11/crates/trusty-mpm/src/new_mod.rs"
expect_refusal bin-fresh-untracked 1 \
  "is older than its newest source $SRC11/crates/trusty-mpm/src/new_mod.rs" \
  --bin "$TGT11/debug/tm-mid" --dir "$DIR2" --dry-run
rm "$SRC11/crates/trusty-mpm/src/new_mod.rs"
SRC_ROOT_ENV=("SANDBOX_SRC_ROOT=$SRC11" "BIN_FRESH_TEST=1" "BIN_FRESH_SOURCES_CMD=$TREE11-deps")
expect_refusal bin-fresh-dependency 1 \
  "is older than its newest source $SRC11/crates/trusty-secrets/src/lib.rs" \
  --bin "$TGT11/debug/tm-mid" --dir "$DIR2" --dry-run
# The override is announced; without BIN_FRESH_TEST it is ignored with a
# warning, so a stub that would mark tm-mid fresh does not, and a stale binary
# is still refused by the real derivation (cargo cannot read this root).
SRC_ROOT_ENV=("SANDBOX_SRC_ROOT=$SRC11" "BIN_FRESH_TEST=1" "BIN_FRESH_SOURCES_CMD=$TREE11")
expect_refusal bin-fresh-override-notice 0 "sources from BIN_FRESH_SOURCES_CMD override: $TREE11" \
  --bin "$TGT11/debug/tm-mid" --dir "$DIR2" --dry-run
SRC_ROOT_ENV=("SANDBOX_SRC_ROOT=$SRC11" "BIN_FRESH_SOURCES_CMD=$TREE11")
expect_refusal bin-fresh-override-ignored 1 "warning: BIN_FRESH_SOURCES_CMD is set but ignored" \
  --bin "$TGT11/debug/tm-mid" --dir "$DIR2" --dry-run
expect_refusal bin-fresh-override-ignored-stale 1 "refused: cannot judge the freshness of $TGT11/debug/tm" \
  --bin "$TGT11/debug/tm" --dir "$DIR2" --dry-run
SRC_ROOT_ENV=("SANDBOX_SRC_ROOT=$SRC11" "BIN_FRESH_TEST=1" "BIN_FRESH_SOURCES_CMD=false")
expect_refusal bin-fresh-derive-fails 1 "cannot judge the freshness" \
  --bin "$TGT11/release/tm" --dir "$DIR2" --dry-run
mkdir -p "$TMP_ROOT/not-a-repo"
SRC_ROOT_ENV=("SANDBOX_SRC_ROOT=$TMP_ROOT/not-a-repo" "BIN_FRESH_TEST=1" "BIN_FRESH_SOURCES_CMD=$TREE11")
expect_refusal bin-fresh-no-source 1 "no source file of trusty-mpm found by git" \
  --bin "$TGT11/release/tm" --dir "$DIR2" --dry-run
SRC_ROOT_ENV=()

# ── #10721: --credential NAME=<ref> ─────────────────────────────────────────
# A perl stub `tm`: perl adds no variable of its own, so the daemon side sees
# exactly what the wrapper passed. `secrets exec --env NAME=REF -- CMD...`
# sets NAME to a canary (never on argv) and opens a real fd named by
# TRUSTY_SECRETS_TOKEN_FD, as the real exec does, then execs CMD; REF `unresolvable` exits 1 running nothing.
# `daemon ...` records names, args, cwd, `ps` argv and whether the credential
# arrived — never its value.
CANARY="10721-selftest-canary-credential-value"
CRED_STUB="$TMP_ROOT/stub-cred-tm"
cat > "$CRED_STUB" <<'CRED_EOF'
#!/usr/bin/perl
use strict;
use warnings;
use Fcntl qw(F_SETFD);
use POSIX ();
my $canary = "10721-selftest-canary-credential-value";
if (@ARGV && $ARGV[0] eq 'secrets') {
  my ($name, $ref) = $ARGV[3] =~ /^([^=]+)=(.*)$/ or exit 2;
  if ($ref eq 'unresolvable') {
    print STDERR "tm secrets exec: a key did not resolve; nothing ran\n";
    exit 1;
  }
  $ENV{$name} = $canary;
  # A real open fd, close-on-exec cleared, as exec_token.rs leaves the token.
  open(my $tok, '<', $0) or exit 2;
  fcntl($tok, F_SETFD, 0) or exit 2;
  $ENV{TRUSTY_SECRETS_TOKEN_FD} = fileno($tok);
  exec { $ARGV[5] } @ARGV[5 .. $#ARGV] or exit 127;
}
# Before this stub opens anything: every fd above 2 the daemon inherited.
# perl itself holds none here (it closed the script after compiling it).
my @fds;
for my $n (3 .. 1023) {
  my $d = POSIX::dup($n);
  next unless defined $d;
  POSIX::close($d);
  push @fds, $n;
}
my $h = $ENV{HOME};
open(my $ff, '>', "$h/stub-fds") or die;
print $ff join(' ', @fds), "\n";
close $ff;
open(my $f, '>', "$h/stub-env-names") or die;
print $f join("\n", sort keys %ENV), "\n";
close $f;
open($f, '>', "$h/stub-args") or die;
print $f join("\n", @ARGV), "\n";
close $f;
system("pwd -P > '$h/stub-pwd'");
system("ps -A -ww -o args= > '$h/stub-ps'");
my $got = (grep { defined $ENV{$_} && $ENV{$_} eq $canary } keys %ENV) ? 'present' : 'absent';
open($f, '>', "$h/stub-cred") or die;
print $f "$got\n";
close $f;
CRED_EOF
chmod +x "$CRED_STUB"

# cred-forwarded-in-step: the wrapper keeps the launcher's FORWARDED list.
WRAPPER="$SCRIPT_DIR/lib/sandbox_exec.sh"
fwd_list() { sed -n '/^FORWARDED="/,/"$/p' "$1" | sed -e 's/^FORWARDED="//' | tr -d '\\"' | tr ' ' '\n' | grep . | sort; }
if [ "$(fwd_list "$LAUNCHER")" = "$(fwd_list "$WRAPPER")" ] && [ -n "$(fwd_list "$WRAPPER")" ]; then
  pass cred-forwarded-in-step
else
  fail cred-forwarded-in-step "FORWARDED differs between the launcher and $WRAPPER"
fi

# cred-allowlist + cred-canary: a real (stubbed) run with a polluted caller,
# an exported shell function, and exported names that shadow the wrapper's own
# variables.
DIRC="$TMP_ROOT/cred1"
mkdir -p "$DIRC"
set +e
OUTC="$(env -i PATH="$PATH" LANG=C RUST_LOG=warn TELEGRAM_BOT_TOKEN="$FAKE_TOKEN" \
  OPENAI_API_KEY="$FAKE_KEY" OPENROUTER_API_KEY="$FAKE_KEY" KEEP="$FAKE_KEY" NAME="$FAKE_KEY" \
  'BASH_FUNC_leak%%=() { echo '"$FAKE_KEY"'; }' \
  bash "$LAUNCHER" --bin "$CRED_STUB" --dir "$DIRC" --credential ANTHROPIC_API_KEY=selftest-ref 2>&1)"
STATUSC=$?
set -e
if [ "$STATUSC" -ne 0 ]; then
  fail cred-allowlist "launcher exit $STATUSC"
  printf '%s\n' "$OUTC" | sed 's/^/    /'
elif [ ! -f "$DIRC/home/stub-env-names" ]; then
  fail cred-allowlist "the stub daemon never ran"
else
  GOT="$(tr '\n' ' ' < "$DIRC/home/stub-env-names")"
  WANT="ANTHROPIC_API_KEY HOME LANG PATH RUST_LOG TMUX_TMPDIR TRUSTY_DATA_DIR_OVERRIDE TRUSTY_SANDBOX "
  if [ "$GOT" != "$WANT" ]; then
    fail cred-allowlist "daemon saw [$GOT], want [$WANT]"
  elif [ "$(tr '\n' ' ' < "$DIRC/home/stub-args")" != "daemon --sandbox --sandbox-credential ANTHROPIC_API_KEY " ]; then
    fail cred-allowlist "daemon args were [$(tr '\n' ' ' < "$DIRC/home/stub-args")]"
  elif [ "$(cat "$DIRC/home/stub-pwd")" != "$DIRC/home" ]; then
    fail cred-allowlist "daemon cwd was not <dir>/home"
  elif [ "$(cat "$DIRC/home/stub-cred")" != "present" ]; then
    fail cred-allowlist "the credential did not reach the daemon"
  else
    pass cred-allowlist
  fi
fi
# cred-no-inherited-fd: the daemon holds no fd above stderr, so no child of it
# can present the exec's grant token (#10721).
if [ ! -f "$DIRC/home/stub-fds" ]; then
  fail cred-no-inherited-fd "the stub daemon never recorded its fds"
elif [ -n "$(tr -d ' \n' < "$DIRC/home/stub-fds")" ]; then
  fail cred-no-inherited-fd "the daemon inherited fds above 2: $(cat "$DIRC/home/stub-fds")"
else
  pass cred-no-inherited-fd
fi
# The canary is in no output, no ps argv, and no file under the sandbox dir.
if printf '%s' "$OUTC" | grep -qF "$CANARY"; then
  fail cred-canary "the credential value reached the launcher's output"
elif [ ! -s "$DIRC/home/stub-ps" ]; then
  fail cred-canary "no ps argv was captured"
elif grep -rqF "$CANARY" "$DIRC"; then
  fail cred-canary "the credential value is in a file under the sandbox: $(grep -rlF "$CANARY" "$DIRC")"
elif printf '%s' "$OUTC" | grep -qF -e "$FAKE_KEY" -e "$FAKE_TOKEN" -e selftest-ref; then
  fail cred-canary "a fake value or the ref reached the output"
else
  pass cred-canary
fi

# cred-dry-run: names the credential and passes it; starts nothing.
DIRCD="$TMP_ROOT/cred-dry"
mkdir -p "$DIRCD"
expect_refusal cred-dry-run 0 "names only): HOME PATH TRUSTY_DATA_DIR_OVERRIDE TRUSTY_SANDBOX TMUX_TMPDIR LANG RUST_LOG ANTHROPIC_API_KEY" \
  --bin "$CRED_STUB" --dir "$DIRCD" --credential ANTHROPIC_API_KEY=selftest-ref --dry-run
[ ! -e "$DIRCD/home" ] || fail cred-dry-run "a dry run created $DIRCD/home"

# Refusals before anything starts: each fails on a launcher without --credential.
DIRCR="$TMP_ROOT/cred-refuse"
mkdir -p "$DIRCR"
expect_refusal cred-bad-name 1 "--credential name not allowed" \
  --bin "$CRED_STUB" --dir "$DIRCR" --credential TELEGRAM_BOT_TOKEN=selftest-ref
expect_refusal cred-malformed 1 "--credential takes NAME=<ref>" \
  --bin "$CRED_STUB" --dir "$DIRCR" --credential ANTHROPIC_API_KEY
expect_refusal cred-empty-ref 1 "--credential ANTHROPIC_API_KEY has an empty reference" \
  --bin "$CRED_STUB" --dir "$DIRCR" --credential ANTHROPIC_API_KEY=
expect_refusal cred-twice 1 "--credential may be given once" \
  --bin "$CRED_STUB" --dir "$DIRCR" --credential ANTHROPIC_API_KEY=a --credential OPENROUTER_API_KEY=b
[ ! -e "$DIRCR/home" ] || fail cred-refusals-start-nothing "a refused --credential created $DIRCR/home"

# cred-unresolvable: `tm secrets exec` exits 1 and no daemon runs.
DIRCU="$TMP_ROOT/cred-unres"
mkdir -p "$DIRCU"
expect_refusal cred-unresolvable 1 "a key did not resolve" \
  --bin "$CRED_STUB" --dir "$DIRCU" --credential OPENROUTER_API_KEY=unresolvable
[ ! -e "$DIRCU/home/stub-args" ] || fail cred-unresolvable "a daemon ran on an unresolvable ref"

# cred-default-closed: a provider key in the caller env, no --credential:
# the daemon sees none and gets no --sandbox-credential.
DIRCN="$TMP_ROOT/cred-none"
mkdir -p "$DIRCN"
set +e
env -i PATH="$PATH" ANTHROPIC_API_KEY="$CANARY" bash "$LAUNCHER" --bin "$STUB" --dir "$DIRCN" >/dev/null 2>&1
set -e
if grep -qx ANTHROPIC_API_KEY "$DIRCN/home/stub-env-names" 2>/dev/null; then
  fail cred-default-closed "a provider key reached the daemon without --credential"
elif [ "$(tr '\n' ' ' < "$DIRCN/home/stub-args" 2>/dev/null)" != "daemon --sandbox " ]; then
  fail cred-default-closed "the daemon args changed without --credential"
else
  pass cred-default-closed
fi

# The wrapper's own refusals, run directly.
run_wrapper() {
  local name="$1" needle="$2" out status
  shift 2
  set +e
  out="$(env -i PATH="$PATH" "$@" 2>&1)"
  status=$?
  set -e
  if [ "$status" -eq 1 ] && printf '%s' "$out" | grep -qF -- "$needle"; then
    pass "$name"
  else
    fail "$name" "exit $status, output: $out"
  fi
}
run_wrapper cred-wrapper-bad-name "credential name not allowed: GITHUB_PAT" \
  GITHUB_PAT="$FAKE_KEY" bash "$WRAPPER" "$DIRCR" "$CRED_STUB" GITHUB_PAT
run_wrapper cred-wrapper-absent "ANTHROPIC_API_KEY is not set" \
  bash "$WRAPPER" "$DIRCR" "$CRED_STUB" ANTHROPIC_API_KEY
run_wrapper cred-wrapper-bad-fd "TRUSTY_SECRETS_TOKEN_FD is not an fd number" \
  ANTHROPIC_API_KEY="$FAKE_KEY" TRUSTY_SECRETS_TOKEN_FD='3;x' bash "$WRAPPER" "$DIRCR" "$CRED_STUB" ANTHROPIC_API_KEY
run_wrapper cred-wrapper-stdio-fd "TRUSTY_SECRETS_TOKEN_FD names stdio" \
  ANTHROPIC_API_KEY="$FAKE_KEY" TRUSTY_SECRETS_TOKEN_FD=2 bash "$WRAPPER" "$DIRCR" "$CRED_STUB" ANTHROPIC_API_KEY

echo "sandbox_daemon selftest: $PASSED passed, $FAILED failed"
[ "$FAILED" -eq 0 ]
