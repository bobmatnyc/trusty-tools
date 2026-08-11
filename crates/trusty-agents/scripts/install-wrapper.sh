#!/usr/bin/env bash
# Install the tagent wrapper script to ~/.local/bin and ~/.cargo/bin.
# Called by `make install`. Substitutes __PROJECT_DIR__ with the actual path.

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

install_wrapper "${HOME}/.local/bin/tagent"

# ~/.cargo/bin takes PATH precedence (rustup puts it first), so install there too.
if [[ -d "${HOME}/.cargo/bin" ]]; then
  install_wrapper "${HOME}/.cargo/bin/tagent"
fi

echo "Binary:  ${PROJECT_DIR}/target/release/tagent"
