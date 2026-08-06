#!/usr/bin/env bash
# scripts/install-trusty-memory-signed.sh
#
# Why: Every `cargo install trusty-memory` produces ad-hoc / linker-signed
# binaries with a fresh cdhash. macOS TCC keys grants by cdhash, so every
# category trusty-memory has triggered is revoked on each rebuild and the
# operator is re-prompted — the same cdhash-instability class as
# trusty-search's Full Disk Access churn (#873), trusty-mpm's App-Data churn
# (#2721), and tagent's (#4277). trusty-memory was left out of the
# `SIGNABLE_BINARIES` table for no recorded reason; the owner ruled on
# 2026-08-06 that it belongs there, and this script is its local-source
# install path.
#
# The TCC surface is NOT trusty-memory's own store
# (`~/Library/Application Support/trusty-memory/` — an app's own data
# directory is not protected). It is the `$HOME` walk: `trusty-memory setup`
# and `trusty-memory migrate` call
# `trusty_common::claude_config::discover_claude_settings`, which recurses
# `$HOME` to depth 8 looking for `.claude/settings*.json` and rewrites the
# ones it finds. That walk's skip-list covers `Library` and `Applications`
# but NOT `~/Desktop`, `~/Documents`, or `~/Downloads`, so a `read_dir` is
# attempted on each — the macOS Files-and-Folders TCC categories.
#
# What: Builds+installs trusty-memory from local source (`cargo install
# --path crates/trusty-memory --locked`), then codesigns ALL THREE installed
# binaries — `trusty-memory` (`com.trusty.trusty-memory`),
# `trusty-bm25-daemon` (`com.trusty.trusty-bm25-daemon`), and the deprecated
# `trusty-memory-mcp-bridge` shim (`com.trusty.trusty-memory-mcp-bridge`) —
# by shelling out to `tctl sign trusty-memory`, the single source of truth for
# trusty-* codesign flags and identifiers
# (`crates/trusty-installer/src/commands/macos_signing/mod.rs`, #2558). This
# script duplicates NO codesign logic, mirroring
# `install-trusty-agents-signed.sh`'s `resolve_tctl()` delegation exactly.
#
# Signing all three rather than just the primary binary is the #2721 lesson
# applied directly: signing part of an install set leaves the rest ad-hoc and
# the prompt keeps recurring.
#
# Hardened Runtime: `tctl sign` always applies `--options runtime --timestamp`
# for an EXPLICIT operator signing request (this script's path), matching
# trusty-search. The AUTOMATIC `tctl install` hook does not, because
# trusty-memory links `ort`/`fastembed` through `trusty-common`'s
# `memory-core` feature and ONNX-dylib loading under Hardened Runtime's
# library validation has not been empirically verified — see
# `macos_signing::use_hardened_runtime`, which puts `MEMORY_SET` on
# `SEARCH_SET`'s conservative side for exactly that reason.
#
# Fail-soft requirement (matching install-trusty-agents-signed.sh, not the
# hard-failing mpm/search scripts): a missing Developer ID certificate must
# not abort the run.
#   1. `cargo install --path crates/trusty-memory --locked` always runs and,
#      if IT fails, that is a real build failure — the script exits non-zero.
#   2. The signing step (`tctl sign trusty-memory`) is wrapped so a non-zero
#      exit (no cert, or any other codesign failure) becomes a clear stderr
#      warning and the script continues, exiting 0 overall.
#
# Test: `bash -n scripts/install-trusty-memory-signed.sh` for syntax check.
# `shellcheck scripts/install-trusty-memory-signed.sh` for lint.
# `--dry-run` (or `TRUSTY_CODESIGN_DRY_RUN=1`) prints every cargo/tctl command
# WITHOUT executing it. Run on a machine without a Developer ID cert to
# validate the fail-soft warning path and confirm the script still exits 0.
#
# Usage:
#   # Standard: build from repo source (default) and sign all three binaries
#   scripts/install-trusty-memory-signed.sh
#
#   # Dry-run: print the commands without installing or signing anything
#   scripts/install-trusty-memory-signed.sh --dry-run
#
#   # Override signing identity (e.g. multiple Developer ID certs)
#   TRUSTY_SIGN_IDENTITY="Developer ID Application: Acme Corp (ABCDE12345)" \
#     scripts/install-trusty-memory-signed.sh
#
#   # Override cargo source path (defaults to the repo this script lives in)
#   TRUSTY_INSTALL_PATH=/path/to/trusty-tools scripts/install-trusty-memory-signed.sh

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration (all overridable via environment)
# ---------------------------------------------------------------------------

CARGO_BIN_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"
readonly MEMORY_BIN="$CARGO_BIN_DIR/trusty-memory"
readonly BM25_BIN="$CARGO_BIN_DIR/trusty-bm25-daemon"
readonly BRIDGE_BIN="$CARGO_BIN_DIR/trusty-memory-mcp-bridge"

# Sign identity: auto-detected by `tctl sign` if not overridden. Exported so
# the child `tctl` process honours the same override this script was given.
TRUSTY_SIGN_IDENTITY="${TRUSTY_SIGN_IDENTITY:-}"
export TRUSTY_SIGN_IDENTITY

# Install source: path dep (default) — derived from this script's own location.
TRUSTY_INSTALL_PATH="${TRUSTY_INSTALL_PATH:-}"

# Dry-run mode: print commands without executing. Set via env or --dry-run flag.
TRUSTY_CODESIGN_DRY_RUN="${TRUSTY_CODESIGN_DRY_RUN:-0}"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# All log helpers write to stderr (issue #2322): functions called via command
# substitution must not leak stdout output into the captured value.
info()    { printf '\033[0;32m[install-memory-signed] %s\033[0m\n' "$*" >&2; }
warn()    { printf '\033[0;33m[install-memory-signed] WARNING: %s\033[0m\n' "$*" >&2; }
error()   { printf '\033[0;31m[install-memory-signed] ERROR: %s\033[0m\n' "$*" >&2; }
section() { printf '\n\033[1;34m==> %s\033[0m\n' "$*" >&2; }

# Detect repo root from the script's own location (NOT $PWD).
# Why: `cargo install --path` needs an absolute path to the workspace root.
detect_repo_root() {
    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    dirname "$script_dir"
}

# Resolve the `tctl` (or `trusty-installer`) binary to run the sign step.
# Why: The unified signing logic (#2558) lives in trusty-installer; this
# script must not duplicate `codesign`/`security find-identity` calls. Prefers
# an already-installed `tctl`/`trusty-installer` on PATH; falls back to
# `cargo run -p trusty-installer` from the detected repo root so a fresh
# checkout with no prior tctl install still works. Mirrors
# `install-trusty-agents-signed.sh`'s `resolve_tctl()` exactly.
resolve_tctl() {
    if command -v tctl >/dev/null 2>&1; then
        echo "tctl"
        return 0
    fi
    if command -v trusty-installer >/dev/null 2>&1; then
        echo "trusty-installer"
        return 0
    fi
    return 1
}

# ---------------------------------------------------------------------------
# Step 1: cargo install (hard requirement — failure here is a real build
# failure, not the fail-soft signing case this script exists for)
# ---------------------------------------------------------------------------

run_cargo_install() {
    section "Step 1: cargo install trusty-memory (from local source)"

    local repo_root crate_path
    if [[ -n "$TRUSTY_INSTALL_PATH" ]]; then
        repo_root="$TRUSTY_INSTALL_PATH"
    else
        repo_root="$(detect_repo_root)"
    fi
    crate_path="$repo_root/crates/trusty-memory"

    if [[ "$TRUSTY_CODESIGN_DRY_RUN" == "1" ]]; then
        info "[DRY RUN] cargo install --path $crate_path --locked"
        return 0
    fi

    info "Installing trusty-memory from: $crate_path"
    cargo install --path "$crate_path" --locked

    info "cargo install complete. Installed binaries:"
    local bin
    for bin in "$MEMORY_BIN" "$BM25_BIN" "$BRIDGE_BIN"; do
        if [[ -f "$bin" ]]; then
            printf '  %s\n' "$bin" >&2
        else
            # The mcp-bridge shim is deprecated and may legitimately be absent
            # on a future release that drops it; `tctl sign` skips any binary
            # that is not on disk, so this is a note, not a failure.
            warn "$bin not found after cargo install"
        fi
    done
}

# ---------------------------------------------------------------------------
# Step 2: sign the set (delegates to `tctl sign trusty-memory` — #2558)
# fail-soft: a signing failure is a WARNING, not a script-aborting error.
# ---------------------------------------------------------------------------

run_sign() {
    section "Step 2: Codesign the trusty-memory set via 'tctl sign trusty-memory'"

    if [[ "$TRUSTY_CODESIGN_DRY_RUN" == "1" ]]; then
        info "[DRY RUN] Would run: tctl sign trusty-memory --dir $CARGO_BIN_DIR"
        info "[DRY RUN]   trusty-memory            -> com.trusty.trusty-memory"
        info "[DRY RUN]   trusty-bm25-daemon       -> com.trusty.trusty-bm25-daemon"
        info "[DRY RUN]   trusty-memory-mcp-bridge -> com.trusty.trusty-memory-mcp-bridge"
        return 0
    fi

    local tctl_bin repo_root rc=0
    if tctl_bin="$(resolve_tctl)"; then
        info "Using $tctl_bin on PATH"
        "$tctl_bin" sign trusty-memory --dir "$CARGO_BIN_DIR" || rc=$?
    else
        repo_root="$(detect_repo_root)"
        warn "tctl/trusty-installer not found on PATH — running via 'cargo run -p trusty-installer'"
        warn "(install trusty-installer once — cargo install --path $repo_root/crates/trusty-installer --locked — to skip this rebuild next time)"
        (cd "$repo_root" && cargo run --quiet -p trusty-installer -- sign trusty-memory --dir "$CARGO_BIN_DIR") || rc=$?
    fi

    if [[ "$rc" -ne 0 ]]; then
        warn "Signing the trusty-memory set failed (exit $rc) — the binaries remain ad-hoc signed."
        warn "macOS will re-prompt for any TCC-gated permission after the next rebuild — this"
        warn "is EXPECTED on CI or any machine without a Developer ID Application cert."
        warn "See docs/reference/release-workflow.md for one-time cert setup."
        return 0
    fi
    info "trusty-memory set signed successfully."
}

# ---------------------------------------------------------------------------
# Post-install guidance
# ---------------------------------------------------------------------------

print_next_steps() {
    section "Done — next steps"
    # Literal "$HOME" and "~/..." below are narration for the reader, not
    # paths for the shell to expand.
    # shellcheck disable=SC2016,SC2088
    {
    printf '\ntrusty-memory needs NO Full Disk Access re-grant — its palace store lives\n'
    printf 'under $HOME, and an application data directory is not TCC-protected.\n'
    printf 'What this script fixes is the Files-and-Folders category reached by\n'
    printf '"trusty-memory setup" / "trusty-memory migrate", which walk $HOME for\n'
    printf '.claude/settings*.json and so touch ~/Desktop, ~/Documents, and\n'
    printf '~/Downloads. Approve those prompts once — the stable Developer ID\n'
    printf 'identity makes the approval persist across every future cargo install.\n\n'
    }
    printf 'RESTART the trusty-memory daemon gracefully to pick up the newly signed\n'
    printf 'binaries (SIGTERM drains in-flight requests; do NOT use kickstart -k):\n'
    printf '  tctl restart trusty-memory\n'
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

main() {
    for arg in "$@"; do
        case "$arg" in
            --dry-run) TRUSTY_CODESIGN_DRY_RUN=1 ;;
            -h|--help)
                # Print only the header comment block (Why/What/.../Usage),
                # not every inline comment in the script body: stop at the
                # first non-comment line.
                awk 'NR==1{next} /^#/{sub(/^# ?/,""); print; next} {exit}' "${BASH_SOURCE[0]}" >&2
                exit 0
                ;;
            *)
                error "Unknown argument: $arg (supported: --dry-run, --help)"
                exit 2
                ;;
        esac
    done

    printf '\033[1m=== trusty-memory signed install ===\033[0m\n\n' >&2
    if [[ "$TRUSTY_CODESIGN_DRY_RUN" == "1" ]]; then
        warn "DRY RUN — no cargo install, no signing."
    fi

    if [[ "$(uname)" != "Darwin" ]]; then
        error "This script is macOS-only (codesign / TCC are Apple-specific)."
        exit 1
    fi

    run_cargo_install
    run_sign
    print_next_steps
}

main "$@"
