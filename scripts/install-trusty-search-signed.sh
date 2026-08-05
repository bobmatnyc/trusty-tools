#!/usr/bin/env bash
# scripts/install-trusty-search-signed.sh
#
# Why: Every `cargo install trusty-search` produces a binary with a new cdhash
# (ad-hoc / linker-signed), causing macOS TCC to revoke the Full Disk Access
# grant — the user must re-grant FDA manually after every reinstall (#873).
# Signing with a stable Developer ID Application identity + fixed --identifier
# per binary makes the designated requirement (DR) stable across rebuilds, so
# the FDA grant persists permanently.
#
# What: Runs `cargo install` for trusty-search from local source (which also
# installs the bundled trusty-embedderd binary) — this script's one job that
# `tctl install` cannot replace, since `tctl install` only pulls prebuilt or
# crates.io releases, never a local working tree. The actual codesign +
# identity-detection + verification step (previously duplicated here in bash)
# is now delegated to `tctl sign trusty-search` — the single source of truth
# in `crates/trusty-installer/src/commands/macos_signing.rs` (#2558). This
# closes the drift between this script and the `tctl install` post-install
# hook (they previously used different --identifier values and signing flags).
#
# Test: Run `bash -n scripts/install-trusty-search-signed.sh` for syntax check.
# Run with `TRUSTY_CODESIGN_DRY_RUN=1` to exercise the identity-detection and
# guidance paths without running cargo install or `tctl sign`. Run on a machine
# without a Developer ID cert to validate the no-cert error path and exit code.
# Run `shellcheck scripts/install-trusty-search-signed.sh` for lint.
#
# Usage:
#   # Standard: build from repo source (default)
#   scripts/install-trusty-search-signed.sh
#
#   # From crates.io published version
#   TRUSTY_INSTALL_VERSION=0.24.10 scripts/install-trusty-search-signed.sh
#
#   # Override signing identity
#   TRUSTY_SIGN_IDENTITY="Developer ID Application: Acme Corp (ABCDE12345)" \
#     scripts/install-trusty-search-signed.sh
#
#   # Override cargo source path
#   TRUSTY_INSTALL_PATH=/path/to/trusty-tools scripts/install-trusty-search-signed.sh
#
#   # Dry-run: skip cargo install and signing, exercise guidance paths only
#   TRUSTY_CODESIGN_DRY_RUN=1 scripts/install-trusty-search-signed.sh
#
# Sibling entry point: `tctl sign trusty-mpm` covers the same Developer-ID
# signing for the trusty-mpm binary (owner-authorized scope extension, #2558)
# — there is no separate `install-trusty-mpm-signed.sh` script because
# `tctl install trusty-mpm` (not a local-source build) already covers the
# common case; `cargo install --path crates/trusty-mpm --locked && tctl sign
# trusty-mpm` covers the local-source case.

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration (all overridable via environment)
# ---------------------------------------------------------------------------

# Binaries installed by `cargo install trusty-search`
CARGO_BIN_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"
readonly SEARCH_BIN="$CARGO_BIN_DIR/trusty-search"
readonly EMBEDDERD_BIN="$CARGO_BIN_DIR/trusty-embedderd"

# Sign identity: auto-detected by `tctl sign` if not set
TRUSTY_SIGN_IDENTITY="${TRUSTY_SIGN_IDENTITY:-}"
export TRUSTY_SIGN_IDENTITY

# Install source: path dep (default) or crates.io version
TRUSTY_INSTALL_PATH="${TRUSTY_INSTALL_PATH:-}"
TRUSTY_INSTALL_VERSION="${TRUSTY_INSTALL_VERSION:-}"

# Dry-run mode: skip cargo install + signing; exercise guidance paths only
TRUSTY_CODESIGN_DRY_RUN="${TRUSTY_CODESIGN_DRY_RUN:-0}"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# All log helpers write to stderr (issue #2322). This is not cosmetic: several
# functions below (detect_repo_root) are called via command substitution
# (`repo_root="$(detect_repo_root)"`), and anything these helpers print to
# stdout gets silently concatenated into the captured value.
info()    { printf '\033[0;32m[install-signed] %s\033[0m\n' "$*" >&2; }
warn()    { printf '\033[0;33m[install-signed] WARNING: %s\033[0m\n' "$*" >&2; }
error()   { printf '\033[0;31m[install-signed] ERROR: %s\033[0m\n' "$*" >&2; }
section() { printf '\n\033[1;34m==> %s\033[0m\n' "$*" >&2; }

# Detect repo root (script may be called from anywhere)
# Why: cargo install --path needs an absolute path to the workspace root.
detect_repo_root() {
    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    # scripts/ lives directly under the workspace root
    dirname "$script_dir"
}

# Resolve the `tctl` (or `trusty-installer`) binary to run the sign step.
# Why: The unified signing logic (#2558) lives in trusty-installer; this
# script must not duplicate `codesign`/`security find-identity` calls. Prefers
# an already-installed `tctl`/`trusty-installer` on PATH; falls back to
# `cargo run -p trusty-installer` from the detected repo root so a fresh
# checkout with no prior tctl install still works.
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
# Step 1: cargo install
# ---------------------------------------------------------------------------

run_cargo_install() {
    section "Step 1: cargo install trusty-search"

    if [[ "$TRUSTY_CODESIGN_DRY_RUN" == "1" ]]; then
        info "[DRY RUN] Skipping cargo install"
        return 0
    fi

    if [[ -n "$TRUSTY_INSTALL_VERSION" ]]; then
        # Install a specific published version from crates.io
        info "Installing trusty-search==${TRUSTY_INSTALL_VERSION} from crates.io..."
        cargo install "trusty-search@${TRUSTY_INSTALL_VERSION}" --locked
    elif [[ -n "$TRUSTY_INSTALL_PATH" ]]; then
        # Install from an explicit path
        info "Installing trusty-search from path: $TRUSTY_INSTALL_PATH"
        cargo install --path "$TRUSTY_INSTALL_PATH/crates/trusty-search" --locked
    else
        # Default: find the repo root relative to this script and install from source
        local repo_root
        repo_root="$(detect_repo_root)"
        info "Installing trusty-search from repo root: $repo_root"
        cargo install --path "$repo_root/crates/trusty-search" --locked
    fi

    info "cargo install complete."
    info "Installed binaries:"
    for bin in "$SEARCH_BIN" "$EMBEDDERD_BIN"; do
        if [[ -f "$bin" ]]; then
            printf '  %s\n' "$bin"
        else
            warn "$bin not found after cargo install (unexpected)"
        fi
    done
}

# ---------------------------------------------------------------------------
# Step 2: sign (delegates to `tctl sign trusty-search` — #2558)
# ---------------------------------------------------------------------------

run_sign() {
    section "Step 2: Codesign via 'tctl sign trusty-search'"

    if [[ "$TRUSTY_CODESIGN_DRY_RUN" == "1" ]]; then
        info "[DRY RUN] Would run: tctl sign trusty-search --dir $CARGO_BIN_DIR"
        return 0
    fi

    local tctl_bin
    if tctl_bin="$(resolve_tctl)"; then
        info "Using $tctl_bin on PATH"
        "$tctl_bin" sign trusty-search --dir "$CARGO_BIN_DIR"
        return $?
    fi

    local repo_root
    repo_root="$(detect_repo_root)"
    warn "tctl/trusty-installer not found on PATH — running via 'cargo run -p trusty-installer'"
    warn "(install trusty-installer once — cargo install --path $repo_root/crates/trusty-installer --locked — to skip this rebuild next time)"
    (cd "$repo_root" && cargo run --quiet -p trusty-installer -- sign trusty-search --dir "$CARGO_BIN_DIR")
}

# ---------------------------------------------------------------------------
# Post-install guidance
# ---------------------------------------------------------------------------

# Why (#4868): this block used to reach the wrong conclusion from the right
# observation. It saw three names in play, picked `com.trusty.trusty-search` as
# canonical because that is what the Rust constant said, and labelled the live
# `com.trusty.search` a drifted alias. The reverse was true — `com.trusty.search`
# is the unit launchd has loaded, and the constant was the thing that had
# drifted. So the hint told the operator to bootout and bootstrap a plist that
# does not exist, which restarted nothing.
#
# It is also the wrong shape of advice. A hand-run bootout/bootstrap pair cannot
# evict a unit registered under a different label, and leaves the daemon down if
# the bootstrap fails. `trusty-search service install` does both correctly —
# it evicts the legacy labels, reloads only when the unit changed, and rolls
# back rather than leaving the service down. Naming one command also removes
# the last place this script could restate a label.
# Test: `bash -n scripts/install-trusty-search-signed.sh`.
print_restart_hint() {
    printf '\nRESTART the daemon to pick up the newly signed binary:\n'
    printf '  trusty-search service install\n'
    printf '\n  (Re-installs the LaunchAgent under its canonical label, evicts\n'
    printf '   any unit an older installer left behind, and leaves the daemon\n'
    printf '   running if the reload fails.)\n'
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

main() {
    printf '\033[1m=== trusty-search signed install (fixes #873) ===\033[0m\n\n'

    # Validate macOS
    if [[ "$(uname)" != "Darwin" ]]; then
        error "This script is macOS-only (codesign / TCC are Apple-specific)."
        exit 1
    fi

    run_cargo_install
    run_sign

    if [[ "$TRUSTY_CODESIGN_DRY_RUN" != "1" ]]; then
        section "Done — next steps"
        print_restart_hint
        printf '\nVerify the daemon loaded all indexes:\n'
        printf '  trusty-search status\n'
        printf '\nOPTIONAL — notarize for distribution to OTHER machines:\n'
        printf '  See docs/reference/release-workflow.md#notarization-appendix\n'
        printf '  (Not required for local FDA persistence.)\n'
    fi
}

main "$@"
