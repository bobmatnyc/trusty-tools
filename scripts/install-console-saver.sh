#!/usr/bin/env bash
# scripts/install-console-saver.sh
#
# Why: `TrustyConsole.saver` is a directory bundle, not a binary on PATH, so the
# workspace's "never `cp`, always `cargo install`" rule does not reach it — that
# rule exists because a `cp` over an on-PATH executable strands the kernel's
# cdhash cache and the next exec is SIGKILL'd. `~/Library/Screen Savers/` holds
# no executables on PATH; macOS's own documented install for a `.saver` IS a copy
# (#6520).
#
# What: builds the bundle (or takes a prebuilt one via `--from`), removes any
# installed copy, `cp -R`s the new one into `~/Library/Screen Savers/`, prints the
# installed path and its codesign verdict, and prints the one manual step that
# cannot be automated. `--uninstall` removes it.
#
# Usage:
#   bash scripts/install-console-saver.sh
#   bash scripts/install-console-saver.sh --from target/console-saver/TrustyConsole.saver
#   bash scripts/install-console-saver.sh --dry-run
#   bash scripts/install-console-saver.sh --uninstall
#
# Test: `--dry-run` prints every rm/cp without touching the filesystem, so the
# path resolution and both branches are exercisable without installing.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST_DIR="$HOME/Library/Screen Savers"
DEST="$DEST_DIR/TrustyConsole.saver"
DEFAULT_SOURCE="$REPO_ROOT/target/console-saver/TrustyConsole.saver"

# #6540: the saver's CFBundleIdentifier, which is also its `defaults` domain and
# its os_log subsystem — the bundle namespace, not a launchd label. Bound once
# here and interpolated into the closing notice so the string is stated once.
readonly SAVER_IDENTIFIER="com.trusty.console.saver"

SOURCE=""
DRY_RUN=0
UNINSTALL=0

# Prints the header comment block — every `#` line after the shebang, stopping at
# the first line that is not a comment. Kept range-free so an edit to the header
# cannot silently spill `set -euo pipefail` into --help.
usage() {
  awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "${BASH_SOURCE[0]}"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --from)      SOURCE="${2:-}"; shift 2 ;;
    --dry-run)   DRY_RUN=1; shift ;;
    --uninstall) UNINSTALL=1; shift ;;
    -h|--help)   usage; exit 0 ;;
    *)           echo "ERROR: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "ERROR: a .saver bundle is macOS-only; this host is $(uname -s)." >&2
  exit 1
fi

run() {
  if [[ "$DRY_RUN" -eq 1 ]]; then
    printf 'DRY-RUN:'; printf ' %q' "$@"; printf '\n'
    return 0
  fi
  "$@"
}

# --- uninstall -------------------------------------------------------------
if [[ "$UNINSTALL" -eq 1 ]]; then
  if [[ -d "$DEST" ]]; then
    run rm -rf "$DEST"
    echo "removed: $DEST"
  else
    echo "not installed: $DEST"
  fi
  echo
  echo "System Settings caches the screen-saver list; it repopulates on reopen."
  exit 0
fi

# --- build or adopt --------------------------------------------------------
if [[ -z "$SOURCE" ]]; then
  echo "==> building (no --from given)"
  if [[ "$DRY_RUN" -eq 1 ]]; then
    echo "DRY-RUN: bash $REPO_ROOT/scripts/build-console-saver.sh"
  else
    bash "$REPO_ROOT/scripts/build-console-saver.sh"
  fi
  SOURCE="$DEFAULT_SOURCE"
fi

if [[ ! -d "$SOURCE" && "$DRY_RUN" -eq 0 ]]; then
  echo "ERROR: no bundle at $SOURCE" >&2
  exit 1
fi

# --- install ---------------------------------------------------------------
run mkdir -p "$DEST_DIR"
if [[ -d "$DEST" ]]; then
  echo "==> replacing existing install"
  run rm -rf "$DEST"
fi
run cp -R "$SOURCE" "$DEST"

echo
if [[ "$DRY_RUN" -eq 1 ]]; then
  echo "DRY-RUN: nothing was installed; the target would be $DEST"
else
  echo "installed: $DEST"
  codesign --verify --deep --strict --verbose=2 "$DEST"
  codesign -dv "$DEST"
fi

cat <<EOF

MANUAL STEP — this cannot be scripted:
  System Settings → Screen Saver → select "TrustyConsole", then Preview.

  The console must be running and reachable at http://127.0.0.1:7788 (or the
  port set with: defaults -currentHost write $SAVER_IDENTIFIER ConsolePort <port>).
  Watch it live with:
    log stream --predicate 'subsystem == "$SAVER_IDENTIFIER"'
EOF
