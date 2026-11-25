#!/usr/bin/env bash
#
# sandbox_install_paths.sh — the `--projects-root` / `--config-dir` options
# every scripts/sandbox_*.sh launcher shares (#11821 P6a). Sourced, never run.
#
# Why: #11821 lets an install move its projects root and config dir. A live
#   check of that needs a sandbox whose paths are moved, and a sandbox daemon
#   inherits no env var that could move them, so the launcher writes the one
#   rung it can: <dir>/home/.trusty-tools/install.toml. The same file, left in a
#   reused --dir, could point the sandbox at the operator's real config dir.
# What: the caller sets SIP_PROJECTS and SIP_CONFIG from its options (empty
#   when not given) and calls
#     sip_check NAME SANDBOX   before its dry-run exit; returns 1 on a refusal
#     sip_write NAME SANDBOX   after it creates <dir>/home; returns 1 on failure
#   sip_check, given either option: needs a real --dir (not the dry-run
#   placeholder), then scripts/lib/sandbox_install_paths.py check admits each
#   value only when absolute and inside <dir> after symlinks resolve. Given
#   neither: an existing <dir>/home/.trusty-tools/install.toml must parse and
#   name only paths inside <dir>. python3 runs only in those two cases, so a
#   launch without the options and without the file is unchanged.
#   sip_write writes install.toml only when an option was given.
# Test: scripts/sandbox_install_paths_selftest.sh.
# Portability: bash 3.2 (macOS) and bash 5 (Linux).

# Builtins only (cd, pwd): a launcher may run on a PATH with no dirname.
case "${BASH_SOURCE[0]}" in */*) SIP_LIB="${BASH_SOURCE[0]%/*}" ;; *) SIP_LIB=. ;; esac
SIP_LIB="$(cd "$SIP_LIB" && pwd)/sandbox_install_paths.py"
# Never read from the caller's environment: only the options set these.
SIP_PROJECTS=""
SIP_CONFIG=""

# sip_args: the python option words for the values the caller set.
sip_args() {
  SIP_WORDS=()
  [ -z "$SIP_PROJECTS" ] || SIP_WORDS+=(--projects-root "$SIP_PROJECTS")
  [ -z "$SIP_CONFIG" ] || SIP_WORDS+=(--config-dir "$SIP_CONFIG")
}

sip_check() {
  local name="$1" sandbox="$2" file="$2/home/.trusty-tools/install.toml"
  if [ -z "$SIP_PROJECTS$SIP_CONFIG" ]; then
    [ "${sandbox#<}" = "$sandbox" ] || return 0
    [ -e "$file" ] || [ -L "$file" ] || return 0
    command -v python3 >/dev/null 2>&1 || { echo "$name: refused: python3 is needed to check $file" >&2; return 1; }
    python3 -I "$SIP_LIB" check --root "$sandbox" --home "$sandbox/home" 2>&1 \
      | sed "s|^refused: |$name: refused: <dir>/home/.trusty-tools/install.toml: |" >&2
    [ "${PIPESTATUS[0]}" -eq 0 ] || return 1
    echo "$name: install paths: from the existing <dir>/home/.trusty-tools/install.toml"
    return 0
  fi
  [ "${sandbox#<}" = "$sandbox" ] || { echo "$name: refused: --projects-root and --config-dir need --dir" >&2; return 1; }
  command -v python3 >/dev/null 2>&1 || { echo "$name: refused: python3 is needed for --projects-root/--config-dir" >&2; return 1; }
  sip_args
  python3 -I "$SIP_LIB" check --root "$sandbox" --home "$sandbox/home" "${SIP_WORDS[@]}" 2>&1 \
    | sed "s|^refused: |$name: refused: |" >&2
  [ "${PIPESTATUS[0]}" -eq 0 ] || return 1
  echo "$name: install paths: projects_root=${SIP_PROJECTS:-<default>} config_dir=${SIP_CONFIG:-<default>} (written to <dir>/home/.trusty-tools/install.toml)"
}

sip_write() {
  local name="$1" sandbox="$2"
  [ -n "$SIP_PROJECTS$SIP_CONFIG" ] || return 0
  sip_args
  python3 -I "$SIP_LIB" write --home "$sandbox/home" "${SIP_WORDS[@]}" \
    || { echo "$name: refused: cannot write <dir>/home/.trusty-tools/install.toml" >&2; return 1; }
}
