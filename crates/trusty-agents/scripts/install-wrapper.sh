#!/usr/bin/env bash
# Install the tagent wrapper script to the canonical cargo bin dir.
# Called by `make install`. Substitutes __PROJECT_DIR__ with the actual path.
#
# #5777 (#4964 Phase 3): this script used to write BOTH ~/.local/bin and
# ~/.cargo/bin on purpose — a documented workaround for the two-destination
# split. Every write path now targets the one canonical directory, so the
# deliberate double-write is gone.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
TEMPLATE="${SCRIPT_DIR}/tagent-wrapper.sh"

install_wrapper() {
  local dest="$1"
  mkdir -p "$(dirname "$dest")"
  sed "s|__PROJECT_DIR__|${PROJECT_DIR}|g" "$TEMPLATE" > "$dest"
  chmod +x "$dest"
  echo "Installed tagent wrapper -> $dest"
}

install_wrapper "${CARGO_HOME:-${HOME}/.cargo}/bin/tagent"

echo "Binary:  ${PROJECT_DIR}/target/release/tagent"
