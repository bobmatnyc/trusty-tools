#!/usr/bin/env bash
# scripts/install-trusty-secrets-signed.sh
#
# Why (#11587, #11786 AC 6): a file-based Keychain item trusts the binary that
# created it by designated requirement. `cargo install` leaves trusty-secrets
# ad-hoc / linker-signed, so its requirement is the cdhash: every upgrade is a
# new app to the Keychain and every item prompts again. A Developer ID
# signature with the fixed identifier below keeps the requirement stable
# across upgrades. An upgrade that skips this step silently reverts the binary
# to ad-hoc, so run this script for every install, not once.
#
# ADR-0043: `~/.cargo/bin` holds registry installs only. Unlike the other
# `*-signed.sh` scripts, this one installs from crates.io with an explicit
# version (`cargo install trusty-secrets --version <v> --locked`) and never
# with `--path`. Signing is an in-place `codesign --force` on that registry
# binary.
#
# What:
#   1. `cargo install trusty-secrets --version <v> --locked --root <root>`
#      (hard-fail). <root> is CARGO_INSTALL_ROOT, else CARGO_HOME, passed
#      explicitly so the signed path is the installed path. A trusty-secrets
#      on PATH elsewhere is a hard failure.
#   2. `tctl sign trusty-secrets --dir <root>/bin` (hard-fail). `tctl`
#      owns every codesign flag and identifier
#      (crates/trusty-installer/src/commands/macos_signing/mod.rs); this
#      script duplicates none of them. `tctl sign` reads the signature back
#      and exits non-zero when it is ad-hoc, has no Team ID, or has a Team ID
#      other than the identity's.
# This script is FAIL-CLOSED, unlike install-trusty-memory-signed.sh: a
# missing certificate or any signing failure exits non-zero and never prints
# success. An unsigned trusty-secrets is the failure this script exists for.
#
# It does not touch the Keychain. Existing items created under the old ad-hoc
# cdhash still trust that cdhash and prompt once after the first signed
# install; that re-grant is an operator step.
#
# Test: `bash -n` and `shellcheck` on this file. `--dry-run` prints the cargo
# and tctl commands without running them. The fail-closed attestation is
# covered in Rust by
# `macos_signing::tests::sign_set_with_fails_closed_on_every_bad_signature`.
#
# Usage:
#   scripts/install-trusty-secrets-signed.sh --version 0.9.3
#   TRUSTY_SECRETS_VERSION=0.9.3 scripts/install-trusty-secrets-signed.sh
#   scripts/install-trusty-secrets-signed.sh --version 0.9.3 --dry-run
#   TRUSTY_SIGN_IDENTITY="Developer ID Application: Acme Corp (ABCDE12345)" \
#     scripts/install-trusty-secrets-signed.sh --version 0.9.3

set -euo pipefail

# #11587 fix round: cargo picks its install root from --root, then
# CARGO_INSTALL_ROOT, then the `install.root` config key, then CARGO_HOME.
# This script resolves the root itself and passes it as `--root`, so the
# binary it signs is exactly the one `cargo install` wrote, whatever
# `install.root` says. Signing `$CARGO_HOME/bin` blindly could sign a stale
# binary there while the new install stayed ad-hoc.
INSTALL_ROOT="${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}"
readonly INSTALL_ROOT
readonly CARGO_BIN_DIR="$INSTALL_ROOT/bin"
readonly SECRETS_BIN="$CARGO_BIN_DIR/trusty-secrets"

# The CODESIGN identifier `tctl sign trusty-secrets` applies, mirrored here
# only to narrate the --dry-run plan. A codesign identifier, not a launchd
# label: changing it changes the designated requirement (#2558).
readonly SECRETS_IDENTIFIER="com.trusty.trusty-secrets"

# Exported so the child `tctl` honours the same identity override.
TRUSTY_SIGN_IDENTITY="${TRUSTY_SIGN_IDENTITY:-}"
export TRUSTY_SIGN_IDENTITY

TRUSTY_SECRETS_VERSION="${TRUSTY_SECRETS_VERSION:-}"
TRUSTY_CODESIGN_DRY_RUN="${TRUSTY_CODESIGN_DRY_RUN:-0}"

info()    { printf '\033[0;32m[install-secrets-signed] %s\033[0m\n' "$*" >&2; }
error()   { printf '\033[0;31m[install-secrets-signed] ERROR: %s\033[0m\n' "$*" >&2; }
section() { printf '\n\033[1;34m==> %s\033[0m\n' "$*" >&2; }

# Resolve `tctl` (or `trusty-installer`) on PATH; no `cargo run` fallback,
# because building tctl from a checkout is a path build this script avoids.
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

run_cargo_install() {
    section "Step 1: cargo install trusty-secrets --version $TRUSTY_SECRETS_VERSION --locked (registry)"
    if [[ "$TRUSTY_CODESIGN_DRY_RUN" == "1" ]]; then
        info "[DRY RUN] cargo install trusty-secrets --version $TRUSTY_SECRETS_VERSION --locked --root $INSTALL_ROOT"
        return 0
    fi
    cargo install trusty-secrets --version "$TRUSTY_SECRETS_VERSION" --locked --root "$INSTALL_ROOT"
    if [[ ! -f "$SECRETS_BIN" ]]; then
        error "$SECRETS_BIN not found after cargo install --root $INSTALL_ROOT."
        exit 1
    fi
    check_path_resolves_to_install
}

# #11587 fix round: fail when PATH runs a different trusty-secrets than the one
# just installed; signing this one would leave the binary that runs unsigned.
check_path_resolves_to_install() {
    local on_path resolved expected
    if ! on_path="$(command -v trusty-secrets)"; then
        info "trusty-secrets is not on PATH; signing $SECRETS_BIN."
        return 0
    fi
    resolved="$(cd "$(dirname "$on_path")" && pwd -P)/$(basename "$on_path")"
    expected="$(cd "$CARGO_BIN_DIR" && pwd -P)/trusty-secrets"
    if [[ "$resolved" != "$expected" ]]; then
        error "PATH runs $resolved, but cargo installed $expected."
        error "Signing $expected would leave the binary that runs unsigned. Fix PATH or"
        error "CARGO_INSTALL_ROOT so they agree, then re-run."
        exit 1
    fi
}

run_sign() {
    section "Step 2: codesign via 'tctl sign trusty-secrets' (fail-closed)"
    if [[ "$TRUSTY_CODESIGN_DRY_RUN" == "1" ]]; then
        info "[DRY RUN] tctl sign trusty-secrets --dir $CARGO_BIN_DIR"
        info "[DRY RUN]   trusty-secrets -> $SECRETS_IDENTIFIER"
        return 0
    fi
    local tctl_bin rc=0
    if ! tctl_bin="$(resolve_tctl)"; then
        error "tctl / trusty-installer not found on PATH; trusty-secrets is installed but ad-hoc signed."
        error "Install tctl from the registry (cargo install trusty-installer --version <v> --locked), then re-run."
        exit 1
    fi
    "$tctl_bin" sign trusty-secrets --dir "$CARGO_BIN_DIR" || rc=$?
    if [[ "$rc" -ne 0 ]]; then
        error "Signing trusty-secrets failed (exit $rc). The binary is NOT Developer-ID signed;"
        error "Keychain items will prompt per item until it is. Exit 2 means this tctl predates"
        error "'tctl sign trusty-secrets' (#11587): upgrade trusty-installer first."
        exit 1
    fi
    info "trusty-secrets signed with Developer ID ($SECRETS_IDENTIFIER) and attested."
}

main() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --version)
                [[ $# -ge 2 ]] || { error "--version needs a value"; exit 2; }
                TRUSTY_SECRETS_VERSION="$2"
                shift 2
                ;;
            --dry-run) TRUSTY_CODESIGN_DRY_RUN=1; shift ;;
            --path)
                error "--path is refused: ADR-0043 allows registry installs only."
                exit 2
                ;;
            -h|--help)
                awk 'NR==1{next} /^#/{sub(/^# ?/,""); print; next} {exit}' "${BASH_SOURCE[0]}" >&2
                exit 0
                ;;
            *)
                error "Unknown argument: $1 (supported: --version <v>, --dry-run, --help)"
                exit 2
                ;;
        esac
    done

    if [[ -z "$TRUSTY_SECRETS_VERSION" ]]; then
        error "A version is required: --version <v> or TRUSTY_SECRETS_VERSION (ADR-0043)."
        exit 2
    fi
    if [[ "$(uname)" != "Darwin" ]]; then
        error "This script is macOS-only (codesign / Keychain are Apple-specific)."
        exit 1
    fi

    run_cargo_install
    run_sign
}

main "$@"
