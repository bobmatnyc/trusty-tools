#!/usr/bin/env bash
#
# sandbox_install_paths_selftest.sh — checks for the --projects-root /
# --config-dir options of every scripts/sandbox_*.sh launcher (#11821 P6a).
#
# Why: the options move a sandbox's install paths through
#   <dir>/home/.trusty-tools/install.toml, the one rung a sandbox daemon's
#   cleared env leaves. A path outside <dir>, or a stale install.toml naming
#   one, would point the sandbox at the operator's real config.
# What: (1) for each launcher, a --dry-run with both paths inside --dir
#   exits 0 and names them, and a --config-dir outside --dir refuses; (2) the
#   tm launcher refuses the options without --dir, a relative path, a symlink
#   escaping <dir>, and a stale install.toml that is broken or names a path
#   outside <dir>, and admits one inside; (3) a real run of the tm launcher
#   and of the computer launcher, each with a stub binary that runs
#   scripts/lib/install_paths.py in the daemon's own env, shows the sandboxed
#   process resolving both moved paths from the install file.
# Usage: scripts/sandbox_install_paths_selftest.sh
# Portability: bash 3.2 (macOS) and bash 5 (Linux).

# Check bodies are single-quoted strings `eval`ed later.
# shellcheck disable=SC2016,SC2034
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="$(cd "$(mktemp -d /tmp/sip.XXXXXX)" && pwd -P)"
trap 'rm -rf "$WORK"' EXIT
FAILS=0

check() {
  if eval "$2"; then echo "ok   - $1"; else echo "FAIL - $1"; FAILS=$((FAILS + 1)); fi
}

STUB="$WORK/bin/stub"
mkdir -p "$WORK/bin" "$WORK/outside"
# The stub records how the install-paths resolver sees its own environment.
cat > "$STUB" <<SH
#!/bin/sh
/usr/bin/env python3 -I "$SCRIPT_DIR/lib/install_paths.py" > "\$HOME/../resolved.txt" 2>&1
exit 0
SH
chmod +x "$STUB"

# launcher_args NAME: the binary options NAME needs for a --dry-run.
launcher_args() {
  case "$1" in
    sandbox_secrets_console) ARGS=(--secrets-bin "$STUB" --console-bin "$STUB") ;;
    sandbox_assistants_console) ARGS=(--console-bin "$STUB" --ta-bin "$STUB" --bind 127.0.0.1:1) ;;
    *) ARGS=(--bin "$STUB") ;;
  esac
  TAIL=()
  [ "$1" != sandbox_assistants_ask ] || TAIL=(personal hi)
}

# (1) every launcher admits paths inside --dir and refuses one outside.
for launcher in sandbox_daemon sandbox_search_daemon sandbox_code_daemon sandbox_secrets_console \
  sandbox_computer_daemon sandbox_assistants_ask sandbox_assistants_console sandbox_memory_daemon; do
  launcher_args "$launcher"
  N=$((${N:-0} + 1)); D="$WORK/d$N"
  mkdir -p "$D"
  OUT="$("$SCRIPT_DIR/$launcher.sh" "${ARGS[@]}" --dir "$D" --projects-root "$D/p" --config-dir "$D/c" \
    --dry-run "${TAIL[@]}" 2>&1)" && RC=0 || RC=$?
  check "$launcher admits paths inside --dir" '[ "$RC" -eq 0 ] && [[ "$OUT" == *"install paths: projects_root=$D/p config_dir=$D/c"* ]]'
  [ "$RC" -eq 0 ] || printf '%s\n' "$OUT" | tail -n 5 | sed 's/^/    /'
  check "$launcher dry run writes nothing" '[ ! -e "$D/home/.trusty-tools/install.toml" ]'
  OUT="$("$SCRIPT_DIR/$launcher.sh" "${ARGS[@]}" --dir "$D" --config-dir "$WORK/outside" \
    --dry-run "${TAIL[@]}" 2>&1)" && RC=0 || RC=$?
  check "$launcher refuses a path outside --dir" '[ "$RC" -eq 1 ] && [[ "$OUT" == *"$launcher: refused: config_dir"*"outside the sandbox dir"* ]]'
done

# (2) the tm launcher's other refusals, and the stale-file guard.
TM="$SCRIPT_DIR/sandbox_daemon.sh"
D="$WORK/tm"
mkdir -p "$D/home/.trusty-tools"
expect_refusal() {
  local label="$1" want="$2"; shift 2
  OUT="$("$TM" --bin "$STUB" "$@" --dry-run 2>&1)" && RC=0 || RC=$?
  check "tm launcher refuses $label" '[ "$RC" -eq 1 ] && [[ "$OUT" == *"$want"* ]]'
}
expect_refusal "the options without --dir" "need --dir" --projects-root "$D/p"
expect_refusal "a relative path" "must be an absolute path" --dir "$D" --projects-root "rel/p"
ln -s "$WORK/outside" "$D/escape"
expect_refusal "a symlink out of --dir" "outside the sandbox dir" --dir "$D" --config-dir "$D/escape/c"
printf 'config_dir = "%s"\n' "$WORK/outside" > "$D/home/.trusty-tools/install.toml"
expect_refusal "a stale install.toml naming a path outside" "install.toml: config_dir" --dir "$D"
printf '[broken\n' > "$D/home/.trusty-tools/install.toml"
expect_refusal "a stale broken install.toml" "install.toml: cannot parse" --dir "$D"
printf 'projects_root = "%s"\n' "$D/p" > "$D/home/.trusty-tools/install.toml"
OUT="$("$TM" --bin "$STUB" --dir "$D" --dry-run 2>&1)" && RC=0 || RC=$?
check "tm launcher admits a stale install.toml inside --dir" '[ "$RC" -eq 0 ] && [[ "$OUT" == *"from the existing"* ]]'

# (3) real runs: the sandboxed process resolves the moved paths.
for launcher in sandbox_daemon sandbox_computer_daemon; do
  D="$WORK/run-$launcher"
  mkdir -p "$D"
  "$SCRIPT_DIR/$launcher.sh" --bin "$STUB" --dir "$D" --projects-root "$D/moved/projects" \
    --config-dir "$D/moved/config" > "$WORK/$launcher.log" 2>&1 || true
  SEEN="$(cat "$D/resolved.txt" 2>/dev/null || true)"
  check "$launcher daemon resolves projects_root from install.toml" \
    '[[ "$SEEN" == *"projects_root	$D/moved/projects	install-file"* ]]'
  check "$launcher daemon resolves config_dir from install.toml" \
    '[[ "$SEEN" == *"config_dir	$D/moved/config	install-file"* ]]'
  check "$launcher created both moved dirs" '[ -d "$D/moved/projects" ] && [ -d "$D/moved/config" ]'
  [ -n "$SEEN" ] || tail -n 5 "$WORK/$launcher.log" | sed 's/^/    /'
done

# (4) #11821 P6b: the writer never follows a symlinked .trusty-tools out of the home.
SIP_PY="$SCRIPT_DIR/lib/sandbox_install_paths.py"
H="$WORK/link-home"
mkdir -p "$H" "$WORK/outside/tools"
ln -s "$WORK/outside/tools" "$H/.trusty-tools"
OUT="$(python3 -I "$SIP_PY" write --home "$H" --config-dir "$H/c" 2>&1)" && RC=0 || RC=$?
check "writer refuses a symlinked .trusty-tools" '[ "$RC" -eq 1 ] && [[ "$OUT" == *"is a symlink"* ]]'
check "writer wrote nothing through the symlink" '[ ! -e "$WORK/outside/tools/install.toml" ]'
H="$WORK/plain-home"
mkdir -p "$H"
OUT="$(python3 -I "$SIP_PY" write --home "$H" --config-dir "$H/c" 2>&1)" && RC=0 || RC=$?
# #11821 P6b: read the mode through python, not `stat -f`; GNU stat treats `-f` as file-system mode and prints a report.
MODE="$(python3 -I -c 'import os, sys; print(format(os.stat(sys.argv[1]).st_mode & 0o777, "o"))' "$H/.trusty-tools/install.toml" 2>/dev/null || true)"
check "writer writes a plain home's install.toml owner-only" '[ "$RC" -eq 0 ] && [ "$MODE" = 600 ]'

if [ "$FAILS" -ne 0 ]; then
  echo "sandbox_install_paths_selftest: $FAILS check(s) failed"
  exit 1
fi
echo "sandbox_install_paths_selftest: all checks passed"
