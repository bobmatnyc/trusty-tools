#!/usr/bin/env bash
# Install the open-mpm wrapper script to ~/.local/bin and ~/.cargo/bin.
# Called by `make install`. Substitutes __PROJECT_DIR__ with the actual path.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
TEMPLATE="${SCRIPT_DIR}/open-mpm-wrapper.sh"

install_wrapper() {
  local dest="$1"
  mkdir -p "$(dirname "$dest")"
  sed "s|__PROJECT_DIR__|${PROJECT_DIR}|g" "$TEMPLATE" > "$dest"
  chmod +x "$dest"
  echo "Installed open-mpm wrapper -> $dest"
}

install_wrapper "${HOME}/.local/bin/open-mpm"

# ~/.cargo/bin takes PATH precedence (rustup puts it first), so install there too.
if [[ -d "${HOME}/.cargo/bin" ]]; then
  install_wrapper "${HOME}/.cargo/bin/open-mpm"
fi

echo "Binary:  ${PROJECT_DIR}/target/release/open-mpm"
