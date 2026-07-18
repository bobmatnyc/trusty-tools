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

# Why: the printed restart hint must name the plist launchd actually has
# installed. The real daemon launchd label is `com.trusty.trusty-search`
# (crates/trusty-search/src/commands/service.rs, LAUNCHD_LABEL) — this script
# already printed the correct name, but the sibling README and the Makefile's
# `deploy` target had drifted to two OTHER names (`com.trusty.search` and
# `com.bobmatnyc.trusty-search` respectively), the same class of bug as #2827.
# What: prefers the canonical `com.trusty.trusty-search.plist` when it exists
# on disk; otherwise globs LaunchAgents for any other `com.trusty.*search*
# .plist` / `com.bobmatnyc.*search*.plist` so the hint still finds a drifted
# label, and falls back to the canonical name if nothing is installed yet.
# Test: `bash -n` syntax check; manually verified against a real
# ~/Library/LaunchAgents/com.trusty.trusty-search.plist during #2834 triage.
resolve_search_plist() {
    local agents_dir="$HOME/Library/LaunchAgents"
    local canonical="$agents_dir/com.trusty.trusty-search.plist"
    if [[ -f "$canonical" ]]; then
        printf '%s' "$canonical"
        return 0
    fi
    local candidate
    for candidate in "$agents_dir"/com.trusty.*search*.plist "$agents_dir"/com.bobmatnyc.*search*.plist; do
        [[ -f "$candidate" ]] || continue
        printf '%s' "$candidate"
        return 0
    done
    printf '%s' "$canonical"
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
        printf '\nRESTART the daemon to pick up the newly signed binary:\n'
        local plist_path
        plist_path="$(resolve_search_plist)"
        # shellcheck disable=SC2016  # literal "$(id -u)" is narration for the reader to run, not expansion here
        printf '  launchctl bootout  gui/$(id -u) %s\n' "$plist_path"
        # shellcheck disable=SC2016
        printf '  launchctl bootstrap gui/$(id -u) %s\n' "$plist_path"
        printf '\nVerify the daemon loaded all indexes:\n'
        printf '  trusty-search status\n'
        printf '\nOPTIONAL — notarize for distribution to OTHER machines:\n'
        printf '  See docs/reference/release-workflow.md#notarization-appendix\n'
        printf '  (Not required for local FDA persistence.)\n'
    fi
}

main "$@"
