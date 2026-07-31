#!/usr/bin/env bash
#
# bake-golden.sh — EXPLORATORY PROBE ARTIFACT (not production tooling)
#
# Bakes a "golden" Tart VM image containing the trusty-tools *toolchain* but
# deliberately containing NO trusty artifacts, no tmux and no claude, so that a
# future install-testing harness can exercise installer auto-install-on-missing
# paths against a known-clean machine.
#
# Provisional layout: scripts/vmtest/. Nothing here is committed to as final.
#
# Usage:
#   scripts/vmtest/bake-golden.sh [--tag TAG] [--base BASE_VM] [--disk-size GB]
#
# Findings that shaped this script (see docs/research/vm-install-probe-findings.md):
#   * mise is ALREADY preinstalled on macos-tahoe-base (Homebrew). Do NOT curl mise.run.
#   * `mise self-update` HARD-FAILS on a Homebrew-managed mise.
#   * mise's rust backend delegates to rustup; ~/.cargo/bin is the real toolchain dir.
#   * Shell activation does NOT make cargo visible to non-interactive shells.
#     `tart exec /bin/sh -c cargo` => 127 even with a login shell. We write ~/.zshenv
#     and drive the guest with /bin/zsh, never /bin/sh.
#   * `tart set --disk-size N` auto-expands the guest APFS container on next boot;
#     no in-guest `diskutil apfs resizeContainer` is required.
#
set -euo pipefail

TART=/opt/homebrew/bin/tart
BASE_VM="tahoe-base"
TAG="$(date +%Y%m%d)"
DISK_SIZE=80
MIN_HOST_FREE_GB=60

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag)       TAG="$2"; shift 2 ;;
    --base)      BASE_VM="$2"; shift 2 ;;
    --disk-size) DISK_SIZE="$2"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

GOLDEN="trusty-toolchain-${TAG}"
LOGDIR="${TMPDIR:-/tmp}/vmtest-bake-${TAG}"
mkdir -p "$LOGDIR"

say()  { printf '\n=== %s ===\n' "$*"; }
fail() { printf '\nBAKE FAILED: %s\n' "$*" >&2; exit 1; }

# Run a script (stdin) inside the guest. ALWAYS zsh: /bin/sh does not read ~/.zshenv,
# so the provisioned toolchain would be invisible.
guest() { "$TART" exec -i "$GOLDEN" /bin/zsh -s; }

# ---------------------------------------------------------------- preflight
say "Preflight"
command -v "$TART" >/dev/null || fail "tart not found at $TART"

HOST_FREE_GB=$(df -g / | awk 'NR==2 {print $4}')
echo "host free disk: ${HOST_FREE_GB} GB (need >= ${MIN_HOST_FREE_GB} GB)"
[[ "$HOST_FREE_GB" -ge "$MIN_HOST_FREE_GB" ]] \
  || fail "insufficient host headroom: ${HOST_FREE_GB} GB free, need ${MIN_HOST_FREE_GB} GB"

"$TART" list | grep -qE "[[:space:]]${BASE_VM}[[:space:]]" \
  || fail "base VM '${BASE_VM}' not found"

if "$TART" list | grep -qE "[[:space:]]${GOLDEN}[[:space:]]"; then
  fail "golden '${GOLDEN}' already exists; delete it or pass a different --tag"
fi

# ---------------------------------------------------------------- clone
say "Clone ${BASE_VM} -> ${GOLDEN}"
t0=$(date +%s)
"$TART" clone "$BASE_VM" "$GOLDEN"
t1=$(date +%s)
CLONE_SEC=$((t1 - t0))
echo "CLONE_SEC=${CLONE_SEC}"

# ---------------------------------------------------------------- resize
say "Resize disk to ${DISK_SIZE} GB"
t0=$(date +%s)
"$TART" set "$GOLDEN" --disk-size "$DISK_SIZE"
t1=$(date +%s)
echo "RESIZE_SEC=$((t1 - t0))"

# ---------------------------------------------------------------- boot
say "Boot ${GOLDEN} headless"
BOOT_T0=$(date +%s)
nohup "$TART" run --no-graphics "$GOLDEN" > "$LOGDIR/run.log" 2>&1 &
RUN_PID=$!
echo "tart run pid=${RUN_PID}"

# Poll from the same process that started the boot, so the timing is honest.
READY_SEC=""
for _ in $(seq 1 300); do
  if "$TART" exec "$GOLDEN" /bin/zsh -c 'exit 0' >/dev/null 2>&1; then
    READY_SEC=$(( $(date +%s) - BOOT_T0 ))
    break
  fi
  sleep 1
done
[[ -n "$READY_SEC" ]] || fail "guest agent never became responsive"
echo "BOOT_TO_READY_SEC=${READY_SEC}"

# ------------------------------------------------- pristine (pre-provision) state
say "Pristine base inventory (BEFORE provisioning)"
guest > "$LOGDIR/pristine.txt" 2>&1 <<'PRISTINE'
echo "## uname / version"
sw_vers; uname -m
echo "## cpu/mem"
echo "logicalcpu=$(sysctl -n hw.logicalcpu) mem_gb=$(( $(sysctl -n hw.memsize)/1073741824 ))"
echo "## disk (after tart --disk-size, checking auto-expansion)"
df -h /
echo "## preinstalled tool presence"
for t in mise brew git gh tmux curl claude rustc cargo rustup uv node python3 jq cmake; do
  p=$(command -v "$t" 2>/dev/null) && echo "PRESENT $t -> $p" || echo "ABSENT  $t"
done
echo "## pre-existing rust state (base image artifact check)"
ls -la ~/.rustup 2>&1 | head -5
ls -la ~/.cargo  2>&1 | head -5
echo "## mise version"
mise --version
PRISTINE
cat "$LOGDIR/pristine.txt"

# ---------------------------------------------------------------- provision
say "Provision toolchain via mise"
PROV_T0=$(date +%s)
guest > "$LOGDIR/provision.txt" 2>&1 <<'PROVISION'
set -e

# NOTE: mise is preinstalled via Homebrew on this base image. We deliberately do
# NOT run `curl https://mise.run | sh` (would create a second, conflicting mise in
# ~/.local/bin) and do NOT run `mise self-update` (hard-fails on package-manager
# installs). Upgrading mise, if ever wanted, is `brew upgrade mise`.
echo "mise: $(mise --version)"

# rust@1.91 == the workspace MSRV floor from the root Cargo.toml.
mise use -g rust@1.91
mise use -g uv@latest

# gh is already present from Homebrew (2.93.0); installing it via mise would be
# redundant, so it is intentionally skipped here.

# The load-bearing bit: make the toolchain visible to NON-INTERACTIVE shells.
# zsh reads ~/.zshenv for every invocation (interactive or not, login or not),
# which is what `ssh host cmd` and `tart exec /bin/zsh` actually use.
cat > ~/.zshenv <<'INNER'
export PATH="$HOME/.cargo/bin:$HOME/.local/share/mise/shims:$PATH"
INNER

echo "## installed:"
mise ls
PROVISION
PROV_T1=$(date +%s)
PROVISION_SEC=$((PROV_T1 - PROV_T0))
cat "$LOGDIR/provision.txt"
echo "PROVISION_SEC=${PROVISION_SEC}"

# ---------------------------------------------------------------- verify
say "Verify toolchain in a fresh NON-INTERACTIVE shell"
guest > "$LOGDIR/verify.txt" 2>&1 <<'VERIFY'
set -e
echo "PATH=$PATH"
echo "rustc: $(rustc --version)"
echo "cargo: $(cargo --version)"
echo "uv:    $(uv --version)"
echo "gh:    $(gh --version | head -1)"
rustc --version | grep -q '1\.91' || { echo "MSRV MISMATCH"; exit 1; }
VERIFY
cat "$LOGDIR/verify.txt"

# ---------------------------------------------------------------- purity gate
say "PURITY ASSERTION"
set +e
guest > "$LOGDIR/purity.txt" 2>&1 <<'PURITY'
# We drive the guest with zsh (see header), and zsh ABORTS on a glob that matches
# nothing instead of leaving the pattern literal the way sh does. Without this the
# purity gate errors out on a *clean* image, i.e. it fails exactly when it should pass.
setopt NULL_GLOB

violations=0
note() { echo "VIOLATION: $*"; violations=$((violations+1)); }

# 1. no trusty binaries anywhere on PATH or in the cargo bin dir
for b in tctl tm tga tcode; do
  p=$(command -v "$b" 2>/dev/null) && note "trusty binary on PATH: $b -> $p"
  [ -e "$HOME/.cargo/bin/$b" ] && note "trusty binary in ~/.cargo/bin: $b"
done
for f in "$HOME"/.cargo/bin/trusty-*; do
  [ -e "$f" ] && note "trusty-* binary in ~/.cargo/bin: $f"
done
command -v 'trusty-search' >/dev/null 2>&1 && note "trusty-search on PATH"

# 2. no ~/.trusty-* state dirs
for d in "$HOME"/.trusty-*; do
  [ -e "$d" ] && note "trusty state dir: $d"
done

# 3. no trusty launch agents
if [ -d "$HOME/Library/LaunchAgents" ]; then
  hits=$(ls "$HOME/Library/LaunchAgents" 2>/dev/null | grep -i trusty)
  [ -n "$hits" ] && note "trusty launch agents: $hits"
fi

# 4. no MCP config
[ -e "$HOME/.mcp.json" ] && note "~/.mcp.json present"

# 5. tmux and claude MUST be absent (harness needs to exercise auto-install)
command -v tmux   >/dev/null 2>&1 && note "tmux present (must stay ABSENT)"
command -v claude >/dev/null 2>&1 && note "claude present (must stay ABSENT)"

if [ "$violations" -gt 0 ]; then
  echo "PURITY_RESULT=FAIL ($violations violations)"
  exit 1
fi
echo "PURITY_RESULT=PASS"
PURITY
PURITY_RC=$?
set -e
cat "$LOGDIR/purity.txt"
[[ $PURITY_RC -eq 0 ]] || fail "purity assertion failed — image contains trusty artifacts"

# ---------------------------------------------------------------- finalize
say "Stop ${GOLDEN}"
t0=$(date +%s)
"$TART" stop "$GOLDEN"
t1=$(date +%s)
echo "STOP_SEC=$((t1 - t0))"
wait "$RUN_PID" 2>/dev/null || true

say "Result"
"$TART" list | grep -E "NAME|${GOLDEN}" || true
DU=$(du -sh "$HOME/.tart/vms/${GOLDEN}" 2>/dev/null | awk '{print $1}')
echo "golden:              ${GOLDEN}"
echo "on-disk (du -sh):    ${DU}"
echo "CLONE_SEC=${CLONE_SEC} BOOT_TO_READY_SEC=${READY_SEC} PROVISION_SEC=${PROVISION_SEC}"
echo "logs: ${LOGDIR}"
