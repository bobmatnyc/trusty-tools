#!/usr/bin/env bash
# scripts/install-trusty-agents-signed.sh
#
# Why: Every `cargo install trusty-agents` produces an ad-hoc / linker-signed
# `tagent` binary with a new cdhash. macOS TCC keys grants by cdhash, so any
# TCC category `tagent` has triggered (it reads `$HOME`/project `.trusty-agents/`
# config and state, and — like `tm` (#2721) — project-local `.claude/` dirs
# that live under other apps' data categories) is revoked on every rebuild,
# re-prompting the operator. Signing with a stable Developer ID Application
# identity + a fixed `--identifier` makes the designated requirement (DR)
# stable across rebuilds, same fix class as trusty-search's FDA churn (#873)
# and trusty-mpm's App-Data churn (#2721). (issue #4277)
#
# What: Builds+installs trusty-agents from local source (`cargo install --path
# crates/trusty-agents --locked`), then codesigns the installed `tagent`
# binary (`~/.cargo/bin/tagent`, `--identifier com.trusty.tagent`) by shelling
# out to `tctl sign trusty-agents` — the single source of truth for trusty-*
# codesign flags/identifiers (`crates/trusty-installer/src/commands/
# macos_signing/mod.rs`, #2558). This script duplicates NO codesign logic,
# mirroring `install-trusty-search-signed.sh`'s `resolve_tctl()` delegation
# pattern exactly.
#
# Second artifact — `Trusty Agents.app` (the Tauri desktop shell,
# `crates/trusty-agents/ui/src-tauri`) — uses a DIFFERENT, DELIBERATELY BETTER
# mechanism than the `trusty-mpm-gui`/`trusty-code-gui` precedent. Those two
# GUIs hardcode `bundle.macOS.signingIdentity` directly in their
# `tauri.conf.json`, which means `cargo tauri build` HARD-FAILS on any machine
# without that exact certificate (see `docs/reference/common-pitfalls.md`).
# `crates/trusty-agents/ui/src-tauri/tauri.conf.json` deliberately leaves
# `signingIdentity` UNSET — Tauri's bundler then falls back to ad-hoc signing
# automatically, so a bare `cargo tauri build` never hard-fails. This script
# instead exports `APPLE_SIGNING_IDENTITY` (Tauri's own bundler env-var
# override — confirmed against the Tauri v2 environment-variables reference:
# "Overwrites tauri.conf.json > bundle > macOS > signingIdentity") ONLY around
# the `.app`-signing step, so: cert present -> Developer-ID-signed bundle;
# cert absent -> the env var is simply not set and Tauri's ad-hoc default
# kicks in, fail-soft for free with no special-casing needed here.
#
# Building the UI bundle (`pnpm install && pnpm tauri build`, which itself
# runs `pnpm build` via `tauri.conf.json`'s `beforeBuildCommand`) is heavy and
# NOT something this script does automatically — mirroring
# `install-trusty-mpm-signed.sh`'s `GUI_BIN` handling exactly: this script only
# looks for an ALREADY-BUILT `Trusty Agents.app` under the conventional Tauri
# output path and signs it in place if found; otherwise it prints an
# informational note (not a warning — an unbuilt GUI is the expected common
# case for a CLI-only install) with the one-liner to build+sign it directly.
#
# Fail-soft requirement (THE key deviation from both sibling scripts, which
# hard-fail under `set -euo pipefail` when signing fails with no cert
# present): this script must NOT abort the whole run just because no
# Developer ID cert is present.
#   1. `cargo install --path crates/trusty-agents --locked` always runs and,
#      if IT fails, that is a real build failure — the script exits non-zero.
#   2. The `tagent` signing step (`tctl sign trusty-agents`) is wrapped so a
#      non-zero exit (no cert, or any other codesign failure) becomes a clear
#      stderr warning and the script continues, exiting 0 overall.
#   3. The `.app` best-effort step is likewise never fatal.
#
# Test: `bash -n scripts/install-trusty-agents-signed.sh` for syntax check.
# `shellcheck scripts/install-trusty-agents-signed.sh` for lint.
# `--dry-run` (or `TRUSTY_CODESIGN_DRY_RUN=1`) prints every cargo/tctl/codesign
# command WITHOUT executing it. Run on a machine without a Developer ID cert
# (or with a deliberately bogus `TRUSTY_SIGN_IDENTITY`) to validate the
# fail-soft warning path and confirm the script still exits 0.
#
# Usage:
#   # Standard: build from repo source (default), sign tagent, best-effort
#   # sign Trusty Agents.app if already built
#   scripts/install-trusty-agents-signed.sh
#
#   # Dry-run: print the commands without installing or signing anything
#   scripts/install-trusty-agents-signed.sh --dry-run
#
#   # Override signing identity (e.g. multiple Developer ID certs)
#   TRUSTY_SIGN_IDENTITY="Developer ID Application: Acme Corp (ABCDE12345)" \
#     scripts/install-trusty-agents-signed.sh
#
#   # Override cargo source path (defaults to the repo this script lives in)
#   TRUSTY_INSTALL_PATH=/path/to/trusty-tools scripts/install-trusty-agents-signed.sh
#
#   # Build + sign the UI bundle directly (not automated by this script):
#   cd crates/trusty-agents/ui && pnpm install && \
#     APPLE_SIGNING_IDENTITY="Developer ID Application: Bob Matsuoka (4JH68XUHC5)" \
#     pnpm tauri build
#   # Output lands in the WORKSPACE root's target/release/bundle/macos/
#   # (crates/trusty-agents/ui/src-tauri is a workspace member, not a nested
#   # sub-workspace with its own target dir).

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration (all overridable via environment)
# ---------------------------------------------------------------------------

CARGO_BIN_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"
readonly TAGENT_BIN="$CARGO_BIN_DIR/tagent"
readonly TAGENT_IDENTIFIER="com.trusty.tagent"
readonly UI_BUNDLE_IDENTIFIER="com.trusty.assistant"
readonly UI_BUNDLE_NAME="Trusty Agents.app"

# Sign identity: auto-detected (by `tctl sign` for tagent, by this script's
# own detect_identity() for the .app bundle) if not overridden.
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
info()    { printf '\033[0;32m[install-agents-signed] %s\033[0m\n' "$*" >&2; }
warn()    { printf '\033[0;33m[install-agents-signed] WARNING: %s\033[0m\n' "$*" >&2; }
error()   { printf '\033[0;31m[install-agents-signed] ERROR: %s\033[0m\n' "$*" >&2; }
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
# `install-trusty-search-signed.sh`'s `resolve_tctl()` exactly.
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

# Auto-detect the Developer ID Application identity from the login keychain,
# for the `.app` bundle path (which does not go through `tctl`). Mirrors
# `install-trusty-mpm-signed.sh`'s `detect_identity()`.
detect_identity() {
    if [[ -n "$TRUSTY_SIGN_IDENTITY" ]]; then
        printf '%s' "$TRUSTY_SIGN_IDENTITY"
        return 0
    fi
    local line ident
    while IFS= read -r line; do
        if [[ "$line" == *"Developer ID Application"* ]]; then
            ident="${line#*\"}"
            ident="${ident%\"}"
            printf '%s' "$ident"
            return 0
        fi
    done < <(security find-identity -v -p codesigning 2>/dev/null || true)
    return 1
}

# ---------------------------------------------------------------------------
# Step 1: cargo install (hard requirement — failure here is a real build
# failure, not the fail-soft signing case this script exists for)
# ---------------------------------------------------------------------------

run_cargo_install() {
    section "Step 1: cargo install trusty-agents (from local source)"

    local repo_root crate_path
    if [[ -n "$TRUSTY_INSTALL_PATH" ]]; then
        repo_root="$TRUSTY_INSTALL_PATH"
    else
        repo_root="$(detect_repo_root)"
    fi
    crate_path="$repo_root/crates/trusty-agents"

    if [[ "$TRUSTY_CODESIGN_DRY_RUN" == "1" ]]; then
        info "[DRY RUN] cargo install --path $crate_path --locked"
        return 0
    fi

    info "Installing trusty-agents from: $crate_path"
    cargo install --path "$crate_path" --locked

    if [[ -f "$TAGENT_BIN" ]]; then
        info "cargo install complete: $TAGENT_BIN"
    else
        warn "$TAGENT_BIN not found after cargo install (unexpected)"
    fi
}

# ---------------------------------------------------------------------------
# Step 2: sign tagent (delegates to `tctl sign trusty-agents` — #2558/#4277)
# fail-soft: a signing failure is a WARNING, not a script-aborting error.
# ---------------------------------------------------------------------------

run_sign_tagent() {
    section "Step 2: Codesign tagent via 'tctl sign trusty-agents'"

    if [[ "$TRUSTY_CODESIGN_DRY_RUN" == "1" ]]; then
        info "[DRY RUN] Would run: tctl sign trusty-agents --dir $CARGO_BIN_DIR (identifier=$TAGENT_IDENTIFIER)"
        return 0
    fi

    local tctl_bin repo_root rc=0
    if tctl_bin="$(resolve_tctl)"; then
        info "Using $tctl_bin on PATH"
        "$tctl_bin" sign trusty-agents --dir "$CARGO_BIN_DIR" || rc=$?
    else
        repo_root="$(detect_repo_root)"
        warn "tctl/trusty-installer not found on PATH — running via 'cargo run -p trusty-installer'"
        warn "(install trusty-installer once — cargo install --path $repo_root/crates/trusty-installer --locked — to skip this rebuild next time)"
        (cd "$repo_root" && cargo run --quiet -p trusty-installer -- sign trusty-agents --dir "$CARGO_BIN_DIR") || rc=$?
    fi

    if [[ "$rc" -ne 0 ]]; then
        warn "Signing tagent failed (exit $rc) — tagent remains ad-hoc signed."
        warn "macOS will re-prompt for any TCC-gated permission on next launch — this"
        warn "is EXPECTED on CI or any machine without a Developer ID Application cert."
        warn "See docs/reference/release-workflow.md for one-time cert setup."
        return 0
    fi
    info "tagent signed successfully (identifier=$TAGENT_IDENTIFIER)."
}

# ---------------------------------------------------------------------------
# Step 3: best-effort sign 'Trusty Agents.app' IF already built.
# Never fatal either way — mirrors install-trusty-mpm-signed.sh's GUI_BIN
# handling: a missing bundle is the expected common case for a CLI-only
# install, not an error.
# ---------------------------------------------------------------------------

find_ui_bundle() {
    local repo_root="$1" candidate
    # Why: `crates/trusty-agents/ui/src-tauri` is an explicit member of the
    # WORKSPACE root Cargo.toml (`members = [..., "crates/trusty-agents/ui/
    # src-tauri"]`) and so shares the workspace's single top-level `target/`
    # directory — confirmed empirically (`pnpm tauri build` from that crate
    # bundles to `<repo_root>/target/{profile}/bundle/macos/`, NOT a nested
    # `ui/src-tauri/target/`). `release` (a real `cargo tauri build`/`pnpm
    # tauri build`) is checked first since that is what an operator actually
    # ships; `debug` (`--debug` / `pnpm tauri build --debug`) is also
    # checked since that is what a quick local verification build produces.
    for candidate in \
        "$repo_root/target/release/bundle/macos/$UI_BUNDLE_NAME" \
        "$repo_root/target/debug/bundle/macos/$UI_BUNDLE_NAME"
    do
        if [[ -d "$candidate" ]]; then
            printf '%s' "$candidate"
            return 0
        fi
    done
    return 1
}

run_sign_ui_bundle() {
    section "Step 3: Best-effort sign '$UI_BUNDLE_NAME' (if already built)"

    local repo_root
    if [[ -n "$TRUSTY_INSTALL_PATH" ]]; then
        repo_root="$TRUSTY_INSTALL_PATH"
    else
        repo_root="$(detect_repo_root)"
    fi

    local bundle_path
    if ! bundle_path="$(find_ui_bundle "$repo_root")"; then
        info "'$UI_BUNDLE_NAME' not found under target/{release,debug}/bundle/macos/ — skipping."
        info "Build + sign it directly with:"
        info "  cd $repo_root/crates/trusty-agents/ui && pnpm install && \\"
        info "    APPLE_SIGNING_IDENTITY=\"\${TRUSTY_SIGN_IDENTITY:-<your Developer ID identity>}\" pnpm tauri build"
        return 0
    fi
    info "Found bundle: $bundle_path"

    if [[ "$TRUSTY_CODESIGN_DRY_RUN" == "1" ]]; then
        info "[DRY RUN] codesign --deep --force --options runtime --timestamp --identifier $UI_BUNDLE_IDENTIFIER --sign \"<identity>\" \"$bundle_path\""
        info "[DRY RUN] codesign --verify --deep --strict \"$bundle_path\""
        return 0
    fi

    local identity
    if ! identity="$(detect_identity)" || [[ -z "$identity" ]]; then
        warn "No Developer ID Application certificate found — '$UI_BUNDLE_NAME' left as-is (whatever Tauri produced, likely ad-hoc)."
        warn "See docs/reference/release-workflow.md for one-time cert setup."
        return 0
    fi

    info "Signing '$bundle_path' (identifier=$UI_BUNDLE_IDENTIFIER)"
    if ! codesign --deep --force --options runtime --timestamp \
        --identifier "$UI_BUNDLE_IDENTIFIER" --sign "$identity" "$bundle_path"; then
        warn "codesign failed for '$bundle_path' — bundle left as-is."
        return 0
    fi
    if ! codesign --verify --deep --strict "$bundle_path"; then
        warn "codesign --verify failed for '$bundle_path' after signing — investigate manually."
        return 0
    fi
    info "'$UI_BUNDLE_NAME' signed and verified."
}

# ---------------------------------------------------------------------------
# Post-install guidance
# ---------------------------------------------------------------------------

print_next_steps() {
    section "Done — next steps"
    # shellcheck disable=SC2016  # literal "$HOME"/".trusty-agents/" is narration, not expansion
    printf '\ntagent / Trusty Agents.app read only project-local ".trusty-agents/" and\n'
    # shellcheck disable=SC2016
    printf '"$HOME/.trusty-agents/" config/state (no external volumes) — analogous to\n'
    printf "trusty-mpm's App-Data category, no Full Disk Access re-grant should be\n"
    printf 'needed. If macOS prompts for data access from other apps (e.g. reading a\n'
    printf 'project-local .claude/ config dir), approve it once — the stable\n'
    printf 'Developer ID identity makes that approval persist across every future\n'
    printf 'cargo install.\n\n'
    printf 'Restart any running tagent process to pick up the newly signed binary.\n'
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

main() {
    for arg in "$@"; do
        case "$arg" in
            --dry-run) TRUSTY_CODESIGN_DRY_RUN=1 ;;
            -h|--help)
                awk 'NR==1{next} /^#/{sub(/^# ?/,""); print; next} {exit}' "${BASH_SOURCE[0]}" >&2
                exit 0
                ;;
            *)
                error "Unknown argument: $arg (supported: --dry-run, --help)"
                exit 2
                ;;
        esac
    done

    printf '\033[1m=== trusty-agents signed install (fixes #4277) ===\033[0m\n\n' >&2
    if [[ "$TRUSTY_CODESIGN_DRY_RUN" == "1" ]]; then
        warn "DRY RUN — no cargo install, no signing."
    fi

    if [[ "$(uname)" != "Darwin" ]]; then
        error "This script is macOS-only (codesign / TCC are Apple-specific)."
        exit 1
    fi

    run_cargo_install
    run_sign_tagent
    run_sign_ui_bundle
    print_next_steps
}

main "$@"
