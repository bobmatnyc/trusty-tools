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
# What: Runs `cargo install` for trusty-search (which also installs the bundled
# trusty-embedderd binary), then codesigns BOTH installed binaries with a
# Developer ID Application identity.  Auto-detects the identity from the login
# keychain; accepts an override via TRUSTY_SIGN_IDENTITY.  Verifies the
# signature and prints actionable guidance for the one-time FDA grant step.
# If no Developer ID cert is found the cargo install still succeeds and the
# script exits non-zero only for the signing step, printing clear setup
# instructions.
#
# Test: Run `bash -n scripts/install-trusty-search-signed.sh` for syntax check.
# Run with `TRUSTY_CODESIGN_DRY_RUN=1` to exercise the identity-detection and
# guidance paths without running cargo install or codesign.  Run on a machine
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
#   # Dry-run: skip cargo install and codesign, exercise guidance paths only
#   TRUSTY_CODESIGN_DRY_RUN=1 scripts/install-trusty-search-signed.sh

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration (all overridable via environment)
# ---------------------------------------------------------------------------

# Identifier bundle IDs — must be stable across rebuilds; do NOT change these
# after the first signed install or the DR will change and FDA will be lost.
readonly SEARCH_IDENTIFIER="com.trusty.trusty-search"
readonly EMBEDDERD_IDENTIFIER="com.trusty.trusty-embedderd"

# Binaries installed by `cargo install trusty-search`
CARGO_BIN_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"
readonly SEARCH_BIN="$CARGO_BIN_DIR/trusty-search"
readonly EMBEDDERD_BIN="$CARGO_BIN_DIR/trusty-embedderd"

# Sign identity: auto-detected if not set
TRUSTY_SIGN_IDENTITY="${TRUSTY_SIGN_IDENTITY:-}"

# Install source: path dep (default) or crates.io version
TRUSTY_INSTALL_PATH="${TRUSTY_INSTALL_PATH:-}"
TRUSTY_INSTALL_VERSION="${TRUSTY_INSTALL_VERSION:-}"

# Dry-run mode: skip cargo install + codesign; exercise guidance paths only
TRUSTY_CODESIGN_DRY_RUN="${TRUSTY_CODESIGN_DRY_RUN:-0}"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

info()    { printf '\033[0;32m[install-signed] %s\033[0m\n' "$*"; }
warn()    { printf '\033[0;33m[install-signed] WARNING: %s\033[0m\n' "$*" >&2; }
error()   { printf '\033[0;31m[install-signed] ERROR: %s\033[0m\n' "$*" >&2; }
section() { printf '\n\033[1;34m==> %s\033[0m\n' "$*"; }

# Detect repo root (script may be called from anywhere)
# Why: cargo install --path needs an absolute path to the workspace root.
detect_repo_root() {
    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    # scripts/ lives directly under the workspace root
    dirname "$script_dir"
}

# Print Developer ID cert setup instructions and exit with the given code.
# Why: A clear, actionable error beats a cryptic codesign failure.
print_cert_setup_and_exit() {
    local exit_code="${1:-1}"
    error "No 'Developer ID Application' certificate found in the login keychain."
    printf >&2 '\n'
    printf >&2 'To enable persistent FDA (one-time setup):\n'
    printf >&2 '\n'
    printf >&2 '  1. Enroll in the Apple Developer Program:\n'
    printf >&2 '       https://developer.apple.com/programs/enroll/\n'
    printf >&2 '\n'
    printf >&2 '  2. Issue a "Developer ID Application" certificate:\n'
    printf >&2 '       Xcode → Settings → Accounts → Manage Certificates →\n'
    printf >&2 '       "+" → "Developer ID Application"\n'
    printf >&2 '       — OR —\n'
    printf >&2 '       https://developer.apple.com/account/resources/certificates/list\n'
    printf >&2 '\n'
    printf >&2 '  3. Download and double-click the .cer file to install it in\n'
    printf >&2 '       your login keychain (Keychain Access will confirm).\n'
    printf >&2 '\n'
    printf >&2 '  4. Verify the cert is present:\n'
    printf >&2 '       security find-identity -v -p codesigning | grep "Developer ID Application"\n'
    printf >&2 '\n'
    printf >&2 '  5. Re-run this script:\n'
    printf >&2 '       scripts/install-trusty-search-signed.sh\n'
    printf >&2 '\n'
    printf >&2 'Note: trusty-search was installed successfully. The ONLY missing step\n'
    printf >&2 'is the Developer ID signing that makes the FDA grant persistent.\n'
    printf >&2 'You can continue using trusty-search now; re-run this script to sign\n'
    printf >&2 'once you have a Developer ID Application certificate.\n'
    exit "$exit_code"
}

# Detect the Developer ID Application identity from the login keychain.
# Why: Avoids hardcoding the Team ID (which varies per developer account).
# What: Parses `security find-identity` output, prefers the TRUSTY_SIGN_IDENTITY
# override, falls back to auto-detection, errors if none found.
detect_identity() {
    if [[ -n "$TRUSTY_SIGN_IDENTITY" ]]; then
        info "Using TRUSTY_SIGN_IDENTITY override: $TRUSTY_SIGN_IDENTITY"
        echo "$TRUSTY_SIGN_IDENTITY"
        return 0
    fi

    info "Auto-detecting Developer ID Application identity..."
    local identities
    identities="$(security find-identity -v -p codesigning 2>/dev/null \
        | grep 'Developer ID Application' \
        | head -1 \
        || true)"

    if [[ -z "$identities" ]]; then
        return 1
    fi

    # Extract the identity string between the first pair of double-quotes
    # Example line: "  1) AABBCC1234 "Developer ID Application: Acme Corp (TEAM1234)""
    local identity
    identity="$(printf '%s' "$identities" | sed 's/.*"\(Developer ID Application[^"]*\)".*/\1/')"

    if [[ -z "$identity" ]]; then
        return 1
    fi

    info "Found identity: $identity"
    echo "$identity"
    return 0
}

# Sign a single binary with the Developer ID identity.
# Why: Encapsulates codesign flags so they are applied identically to both binaries.
# What: Runs codesign --force --options runtime --timestamp --identifier <id> --sign <identity>.
# The --options runtime flag enables the Hardened Runtime, required for notarization.
# The fixed --identifier keeps the DR stable across rebuilds.
sign_binary() {
    local binary="$1"
    local identifier="$2"
    local identity="$3"

    info "Signing $binary"
    info "  identifier: $identifier"
    info "  identity:   $identity"

    codesign \
        --force \
        --options runtime \
        --timestamp \
        --identifier "$identifier" \
        --sign "$identity" \
        "$binary"
}

# Verify the signature on a binary and print the Authority/TeamIdentifier/Identifier.
# Why: Gives the user immediate confirmation that signing succeeded and the DR is stable.
verify_binary() {
    local binary="$1"
    local label="$2"

    info "Verifying signature on $label..."

    # Strict verification (checks all requirements, not just the signature)
    if codesign --verify --strict "$binary" 2>&1; then
        info "  codesign --verify --strict: PASSED"
    else
        error "Signature verification FAILED for $binary"
        return 1
    fi

    # Print the key fields so the user can confirm the DR is correct
    printf '\n  %s signature details:\n' "$label"
    codesign -dvvv "$binary" 2>&1 \
        | grep -E 'Authority|TeamIdentifier|Identifier|Format|Runtime' \
        | sed 's/^/    /' \
        || true
    printf '\n'
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
# Step 2: identity detection
# ---------------------------------------------------------------------------

run_identity_detection() {
    section "Step 2: Detect Developer ID signing identity"

    if [[ "$TRUSTY_CODESIGN_DRY_RUN" == "1" ]]; then
        info "[DRY RUN] Checking for Developer ID identity (no signing will occur)"
    fi

    local identity
    if ! identity="$(detect_identity)"; then
        # cargo install already succeeded; only the signing step is missing.
        # Print setup instructions and exit non-zero for the signing failure.
        warn "cargo install completed successfully."
        print_cert_setup_and_exit 1
    fi

    # Export for use by subsequent steps
    RESOLVED_IDENTITY="$identity"
}

# ---------------------------------------------------------------------------
# Step 3: codesign both binaries
# ---------------------------------------------------------------------------

run_codesign() {
    section "Step 3: Codesign binaries with Developer ID"

    if [[ "$TRUSTY_CODESIGN_DRY_RUN" == "1" ]]; then
        info "[DRY RUN] Would sign:"
        info "  $SEARCH_BIN  (identifier: $SEARCH_IDENTIFIER)"
        info "  $EMBEDDERD_BIN  (identifier: $EMBEDDERD_IDENTIFIER)"
        return 0
    fi

    for bin in "$SEARCH_BIN" "$EMBEDDERD_BIN"; do
        if [[ ! -f "$bin" ]]; then
            warn "$bin not found — skipping (was it built and installed?)"
        fi
    done

    sign_binary "$SEARCH_BIN"    "$SEARCH_IDENTIFIER"    "$RESOLVED_IDENTITY"
    sign_binary "$EMBEDDERD_BIN" "$EMBEDDERD_IDENTIFIER" "$RESOLVED_IDENTITY"

    info "Codesigning complete."
}

# ---------------------------------------------------------------------------
# Step 4: verify signatures
# ---------------------------------------------------------------------------

run_verify() {
    section "Step 4: Verify signatures"

    if [[ "$TRUSTY_CODESIGN_DRY_RUN" == "1" ]]; then
        info "[DRY RUN] Skipping verification"
        return 0
    fi

    verify_binary "$SEARCH_BIN"    "trusty-search"
    verify_binary "$EMBEDDERD_BIN" "trusty-embedderd"
}

# ---------------------------------------------------------------------------
# Step 5: user guidance
# ---------------------------------------------------------------------------

print_next_steps() {
    section "Done — next steps"

    if [[ "$TRUSTY_CODESIGN_DRY_RUN" == "1" ]]; then
        info "[DRY RUN] Completed identity detection path. Run without DRY_RUN to install + sign."
        return 0
    fi

    printf '
Both binaries are now signed with a stable Developer ID.
The designated requirement (DR) is anchored to the signing identity
and the fixed --identifier, so it will NOT change on future reinstalls.

'
    # Detect whether FDA has been granted before (best-effort heuristic)
    local fda_hint=""
    if ! security find-generic-password -a trusty-search -s "trusty-fda-granted" \
         -D "trusty marker" &>/dev/null 2>&1; then
        fda_hint="(one-time)"
    fi

    printf 'FULL DISK ACCESS — %s grant:\n' "${fda_hint:-}"
    printf '  1. Open System Settings → Privacy & Security → Full Disk Access\n'
    printf '  2. Click the "+" button and navigate to:\n'
    printf '       %s\n' "$SEARCH_BIN"
    printf '  3. Enable the toggle for trusty-search.\n'
    printf '\n'
    printf 'After the one-time FDA grant, future "cargo install" rebuilds followed\n'
    printf 'by this script will NOT require re-granting FDA — the DR is now stable.\n'
    printf '\n'
    printf 'RESTART the daemon to pick up the newly signed binary:\n'
    # SC2016: single-quoted intentionally — these are example commands for the user to paste,
    # not expressions to evaluate now. The $(id -u) and <label> are meant to be read literally.
    # shellcheck disable=SC2016
    printf '  launchctl bootout  gui/$(id -u) ~/Library/LaunchAgents/<label>.plist\n'
    # shellcheck disable=SC2016
    printf '  launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/<label>.plist\n'
    printf '\n'
    printf 'Verify the daemon loaded all indexes:\n'
    printf '  trusty-search status\n'
    printf '\n'
    printf 'OPTIONAL — notarize for distribution to OTHER machines:\n'
    printf '  See docs/reference/release-workflow.md#notarization-appendix\n'
    printf '  (Not required for local FDA persistence.)\n'
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
    run_identity_detection
    run_codesign
    run_verify
    print_next_steps
}

main "$@"
