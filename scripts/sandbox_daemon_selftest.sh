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
#     allowlist     the stub sees exactly the four pinned names plus the
#                   forwarded names the caller set (LANG, RUST_LOG), never the
#                   fake secrets or `LC_API_KEY` (plus the names /bin/sh adds
#                   itself), and `daemon --sandbox`
#     no-leak       no fake secret value appears in the launcher's output
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
TMP_ROOT="$(cd "$(mktemp -d)" && pwd -P)"
trap 'rm -rf "$TMP_ROOT"' EXIT

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
STUB_EOF
chmod +x "$STUB"

# The launcher, run from a cleared environment carrying fake secrets, two
# forwardable names, and an `LC_`-prefixed name that is no locale category.
run_launcher() {
  env -i PATH="$PATH" LANG=C RUST_LOG=warn LC_API_KEY="$FAKE_LC" \
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
  WANT="HOME LANG PATH RUST_LOG TRUSTY_DATA_DIR_OVERRIDE TRUSTY_MPM_ADDR "
  if [ "$GOT" != "$WANT" ]; then
    fail allowlist "stub saw [$GOT], want [$WANT]"
  elif [ "$(tr '\n' ' ' < "$DIR1/home/stub-args")" != "daemon --sandbox " ]; then
    fail allowlist "stub args were not 'daemon --sandbox'"
  else
    pass allowlist
  fi
fi
case "$OUT1" in
  *"$FAKE_TOKEN"*|*"$FAKE_KEY"*|*"$FAKE_LC"*) fail no-leak "a fake secret value reached the output" ;;
  *) pass no-leak ;;
esac

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
    | grep -qF "names only): HOME PATH TRUSTY_DATA_DIR_OVERRIDE TRUSTY_MPM_ADDR LANG RUST_LOG"; then
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

echo "sandbox_daemon selftest: $PASSED passed, $FAILED failed"
[ "$FAILED" -eq 0 ]
