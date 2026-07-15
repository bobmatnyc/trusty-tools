#!/usr/bin/env bash
# scripts/install-trusty-mpm-signed.sh
#
# Why: Every `cargo install trusty-mpm` produces fresh ad-hoc / linker-signed
# binaries (`trusty-mpm` AND its `tm` alias) with a new cdhash. macOS TCC keys
# grants by cdhash, so the newer "App Data" / File-Provider access category is
# revoked on every reinstall and Bob is re-prompted with
# `'trusty-mpm' would like to access data from other apps` (#2721 — same
# cdhash-instability class as trusty-search's Full Disk Access churn, #873).
# Signing with a stable Developer ID Application identity + a fixed
# `--identifier` per binary makes the designated requirement (DR) stable across
# rebuilds, so the one-time App-Data approval persists permanently.
#
# What: Builds+installs trusty-mpm from local source
# (`cargo install --path crates/trusty-mpm --locked`), then codesigns BOTH
# installed binaries — `~/.cargo/bin/trusty-mpm` (`--identifier
# com.trusty.trusty-mpm`) and `~/.cargo/bin/tm` (`--identifier com.trusty.tm`) —
# with the Developer ID identity in the login keychain (auto-detected, or the
# `TRUSTY_SIGN_IDENTITY` override), then verifies each signature.
#
# CANONICAL SCHEME NOTE: The single source of truth for trusty-* codesign
# flags/identifiers is `tctl sign` (crates/trusty-installer/src/commands/
# macos_signing.rs, #2558). As of #2721 the `SIGNABLE_BINARIES` table covers the
# FULL trusty-mpm set — both `trusty-mpm` (`com.trusty.trusty-mpm`) AND the `tm`
# binary (`com.trusty.tm`) — so `tctl sign trusty-mpm` and the `tctl install`
# post-install hook now sign both. This script signs the same two binaries
# directly, mirroring that canonical scheme exactly (identifiers, `--options
# runtime --timestamp`, `--verify --deep --strict`, and the identifier-change
# safety notice); it stays as a cert-guidance-friendly wrapper for local-source
# installs and can, if desired, be reduced to a delegation to
# `tctl sign trusty-mpm` now that the table is complete.
#
# Hardened Runtime: `--options runtime --timestamp` is applied to both binaries.
# Unlike trusty-search/trusty-embedderd (which load an ONNX runtime dylib and so
# gate Hardened Runtime carefully — see #2663 / the tctl `use_hardened_runtime`
# split), trusty-mpm loads NO dylibs, so Hardened Runtime's library-validation
# restriction is safe here and matches `tctl sign trusty-mpm`'s behavior.
#
# TCC note: tm needs NO Full Disk Access re-grant — the daemon manages tmux
# sessions and git worktrees under $HOME only. This script fixes the *App-Data /
# File-Provider* TCC category, which is distinct from FDA. FDA re-granting
# guidance remains scoped to trusty-search / external-volume daemons.
#
# Test: `bash -n scripts/install-trusty-mpm-signed.sh` for syntax check.
# `shellcheck scripts/install-trusty-mpm-signed.sh` for lint.
# `--dry-run` (or `TRUSTY_CODESIGN_DRY_RUN=1`) prints every cargo/codesign
# command WITHOUT executing it — exercises path resolution + guidance paths on
# any machine, with or without a Developer ID cert.
#
# Usage:
#   # Standard: build from repo source (default) and sign both binaries
#   scripts/install-trusty-mpm-signed.sh
#
#   # Dry-run: print the commands without installing or signing anything
#   scripts/install-trusty-mpm-signed.sh --dry-run
#
#   # Override signing identity (e.g. multiple Developer ID certs)
#   TRUSTY_SIGN_IDENTITY="Developer ID Application: Acme Corp (ABCDE12345)" \
#     scripts/install-trusty-mpm-signed.sh
#
#   # Override cargo source path (defaults to the repo this script lives in)
#   TRUSTY_INSTALL_PATH=/path/to/trusty-tools scripts/install-trusty-mpm-signed.sh

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration (all overridable via environment)
# ---------------------------------------------------------------------------

# Binaries installed by `cargo install trusty-mpm`.
CARGO_BIN_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"
readonly MPM_BIN="$CARGO_BIN_DIR/trusty-mpm"
readonly TM_BIN="$CARGO_BIN_DIR/tm"

# Fixed codesign identifiers — MUST match the canonical scheme (macos_signing.rs)
# so the designated requirement stays stable across every reinstall.
readonly MPM_IDENTIFIER="com.trusty.trusty-mpm"
readonly TM_IDENTIFIER="com.trusty.tm"

# Sign identity: auto-detected from the keychain if not overridden.
TRUSTY_SIGN_IDENTITY="${TRUSTY_SIGN_IDENTITY:-}"

# Install source: path dep (default) — derived from this script's own location.
TRUSTY_INSTALL_PATH="${TRUSTY_INSTALL_PATH:-}"

# Dry-run mode: print commands without executing. Set via env or --dry-run flag.
TRUSTY_CODESIGN_DRY_RUN="${TRUSTY_CODESIGN_DRY_RUN:-0}"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# All log helpers write to stderr (issue #2322): detect_repo_root() /
# detect_identity() are called via command substitution, and anything printed to
# stdout there gets silently concatenated into the captured value.
info()    { printf '\033[0;32m[install-mpm-signed] %s\033[0m\n' "$*" >&2; }
warn()    { printf '\033[0;33m[install-mpm-signed] WARNING: %s\033[0m\n' "$*" >&2; }
error()   { printf '\033[0;31m[install-mpm-signed] ERROR: %s\033[0m\n' "$*" >&2; }
section() { printf '\n\033[1;34m==> %s\033[0m\n' "$*" >&2; }

# Detect repo root from the script's own location (NOT $PWD) so the script works
# when invoked from a worktree, a symlink, or any cwd.
# Why: `cargo install --path` needs an absolute path to the workspace root.
detect_repo_root() {
    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    # scripts/ lives directly under the workspace root.
    dirname "$script_dir"
}

# Auto-detect the Developer ID Application identity from the login keychain.
# Why: signing needs a stable Developer ID cert; TRUSTY_SIGN_IDENTITY overrides.
# What: honours the env override first; otherwise parses `security find-identity
# -v -p codesigning`, taking the identity string after the ") " token of the
# first "Developer ID Application" line. Echoes the identity on stdout; empty +
# non-zero exit when none found. Mirrors macos_signing::has_developer_id_cert.
detect_identity() {
    if [[ -n "$TRUSTY_SIGN_IDENTITY" ]]; then
        printf '%s' "$TRUSTY_SIGN_IDENTITY"
        return 0
    fi
    local line ident
    while IFS= read -r line; do
        if [[ "$line" == *"Developer ID Application"* ]]; then
            # `security` prints:  1) <SHA1> "Developer ID Application: Name (TEAM)"
            # Extract the quoted common name (between the first and last '"').
            # This is designed to resolve to the same certificate (hence the same
            # Team ID and the same designated requirement) as `tctl sign`'s
            # identity selection. NOTE: tctl's own `has_developer_id_cert`
            # (macos_signing.rs, `line.split(") ").nth(1)`) has a suspected
            # pre-existing parse bug that may pass the fingerprint/quotes through
            # nearly verbatim instead of stripping them — see #2724. This
            # function deliberately strips both, so its output should NOT be
            # assumed byte-identical to tctl's until that's fixed/verified.
            ident="${line#*\"}"
            ident="${ident%\"}"
            printf '%s' "$ident"
            return 0
        fi
    done < <(security find-identity -v -p codesigning 2>/dev/null || true)
    return 1
}

# Warn (loudly) if a binary's CURRENT on-disk identifier differs from the
# canonical one we are about to apply — changing the DR silently invalidates any
# existing TCC grant keyed to the old value (PR #2657 review; #873's symptom).
# Mirrors macos_signing::warn_on_identifier_change.
warn_on_identifier_change() {
    local path="$1" canonical="$2" old
    [[ -f "$path" ]] || return 0
    old="$(codesign -d "$path" 2>&1 | sed -n 's/^Identifier=//p' | head -n1 || true)"
    if [[ -n "$old" && "$old" != "$canonical" ]]; then
        warn "IDENTIFIER CHANGE for $(basename "$path"): $old -> $canonical. Existing macOS App-Data grants keyed to the old identifier will STOP MATCHING after this signing — re-approve the next prompt once."
    fi
}

# Codesign one binary with the canonical flags. Mirrors macos_signing::sign_binary.
sign_binary() {
    local path="$1" identifier="$2" identity="$3"
    if [[ ! -f "$path" ]]; then
        error "$path not found — cannot sign (did cargo install succeed?)."
        return 1
    fi
    warn_on_identifier_change "$path" "$identifier"
    if [[ "$TRUSTY_CODESIGN_DRY_RUN" == "1" ]]; then
        info "[DRY RUN] codesign --force --options runtime --timestamp --identifier $identifier --sign \"$identity\" $path"
        return 0
    fi
    info "Signing $path (identifier=$identifier)"
    codesign --force --options runtime --timestamp \
        --identifier "$identifier" --sign "$identity" "$path"
}

# Verify a signature strictly and display its designated requirement.
# Mirrors macos_signing::verify_signature (+ a human-readable DR dump).
verify_binary() {
    local path="$1"
    if [[ "$TRUSTY_CODESIGN_DRY_RUN" == "1" ]]; then
        info "[DRY RUN] codesign --verify --deep --strict $path"
        info "[DRY RUN] codesign --display --requirements - $path"
        return 0
    fi
    info "Verifying $path"
    codesign --verify --deep --strict "$path"
    # Show the designated requirement so the operator can confirm the DR is
    # anchored to the Team ID + fixed identifier (not the mutable cdhash).
    codesign --display --requirements - "$path" 2>&1 | sed 's/^/    /' >&2 || true
}

# ---------------------------------------------------------------------------
# Step 1: cargo install
# ---------------------------------------------------------------------------

run_cargo_install() {
    section "Step 1: cargo install trusty-mpm (from local source)"

    local repo_root
    if [[ -n "$TRUSTY_INSTALL_PATH" ]]; then
        repo_root="$TRUSTY_INSTALL_PATH"
    else
        repo_root="$(detect_repo_root)"
    fi
    local crate_path="$repo_root/crates/trusty-mpm"

    if [[ "$TRUSTY_CODESIGN_DRY_RUN" == "1" ]]; then
        info "[DRY RUN] cargo install --path $crate_path --locked"
        return 0
    fi

    info "Installing trusty-mpm from: $crate_path"
    cargo install --path "$crate_path" --locked

    info "cargo install complete. Installed binaries:"
    for bin in "$MPM_BIN" "$TM_BIN"; do
        if [[ -f "$bin" ]]; then
            printf '  %s\n' "$bin" >&2
        else
            warn "$bin not found after cargo install (unexpected)"
        fi
    done
}

# ---------------------------------------------------------------------------
# Step 2: sign both binaries
# ---------------------------------------------------------------------------

run_sign() {
    section "Step 2: Codesign trusty-mpm + tm with a stable Developer ID"

    local identity
    if ! identity="$(detect_identity)" || [[ -z "$identity" ]]; then
        error "No 'Developer ID Application' certificate found in the login keychain."
        error "Set up a cert (see docs/reference/release-workflow.md) or set TRUSTY_SIGN_IDENTITY."
        error "Without a stable identity, the App-Data TCC grant cannot persist across reinstalls."
        return 1
    fi
    info "Using signing identity: $identity"

    # trusty-mpm first: if THIS fails, set -e aborts immediately with the raw
    # codesign error and nothing has been re-signed yet — no special messaging
    # needed (the pre-existing state, whatever it was, is untouched).
    sign_binary "$MPM_BIN" "$MPM_IDENTIFIER" "$identity"
    verify_binary "$MPM_BIN"

    # tm second: if THIS fails, trusty-mpm is already re-signed with the new
    # stable DR while tm is not — a HALF-SIGNED state where the two binaries
    # now carry different identities. Guard this step explicitly (inside an
    # `if`, which set -e does not abort on) so we can surface that state
    # clearly instead of just dumping raw codesign stderr and exiting.
    if ! sign_binary "$TM_BIN" "$TM_IDENTIFIER" "$identity" || ! verify_binary "$TM_BIN"; then
        error "HALF-SIGNED STATE: trusty-mpm was signed successfully but tm was NOT."
        error "The two binaries now have different identities (trusty-mpm has the new stable DR; tm is unchanged)."
        error "Re-run this script to retry — signing trusty-mpm again is idempotent (--force overwrites the existing signature)."
        return 1
    fi
}

# ---------------------------------------------------------------------------
# Post-install guidance
# ---------------------------------------------------------------------------

print_next_steps() {
    section "Done — next steps"
    # shellcheck disable=SC2016  # literal "$HOME" is intended narration, not expansion
    printf '\ntrusty-mpm needs NO Full Disk Access re-grant (it reads only $HOME —\n'
    printf 'tmux state + git worktrees). This script fixed the *App-Data / File-\n'
    printf 'Provider* TCC category: approve the next\n'
    printf "  'trusty-mpm' would like to access data from other apps\n"
    printf 'prompt exactly ONCE — the stable Developer ID identity makes that\n'
    printf 'approval persist across every future cargo install.\n\n'
    printf 'RESTART the tm daemon gracefully to pick up the newly signed binaries\n'
    printf '(SIGTERM drains in-flight requests; do NOT use kickstart -k / SIGKILL):\n'
    # shellcheck disable=SC2016
    printf '  launchctl bootout   gui/$(id -u) ~/Library/LaunchAgents/com.trusty.trusty-mpm.plist\n'
    # shellcheck disable=SC2016
    printf '  launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.trusty.trusty-mpm.plist\n'
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

main() {
    # Parse flags (only --dry-run is supported).
    for arg in "$@"; do
        case "$arg" in
            --dry-run) TRUSTY_CODESIGN_DRY_RUN=1 ;;
            -h|--help)
                # Print only the header comment block (Why/What/.../Usage),
                # not every inline comment in the ~300-line script body: stop
                # at the first non-comment line (the blank line right before
                # `set -euo pipefail`).
                awk 'NR==1{next} /^#/{sub(/^# ?/,""); print; next} {exit}' "${BASH_SOURCE[0]}" >&2
                exit 0
                ;;
            *)
                error "Unknown argument: $arg (supported: --dry-run, --help)"
                exit 2
                ;;
        esac
    done

    printf '\033[1m=== trusty-mpm signed install (fixes #2721) ===\033[0m\n\n' >&2
    if [[ "$TRUSTY_CODESIGN_DRY_RUN" == "1" ]]; then
        warn "DRY RUN — no cargo install, no signing, no daemon changes."
    fi

    # macOS-only: codesign / TCC are Apple-specific.
    if [[ "$(uname)" != "Darwin" ]]; then
        error "This script is macOS-only (codesign / TCC are Apple-specific)."
        exit 1
    fi

    run_cargo_install
    run_sign
    print_next_steps
}

main "$@"
