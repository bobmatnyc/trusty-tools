#!/usr/bin/env bash
# scripts/install-trusty-assistants-signed.sh
#
# Why: Every `cargo install trusty-assistants` produces ad-hoc / linker-signed
# `ta` and `tagent` binaries with new cdhashes. macOS TCC keys grants by
# cdhash, so any TCC category the binary has triggered (it reads
# `$HOME/.trusty-assistants/` and project `.trusty-agents/` config and state,
# and — like `tm` (#2721) — project-local `.claude/` dirs that live under other
# apps' data categories) is revoked on every rebuild, re-prompting the
# operator. Signing with a stable Developer ID Application identity + a fixed
# `--identifier` makes the designated requirement (DR) stable across rebuilds,
# same fix class as trusty-search's FDA churn (#873) and trusty-mpm's App-Data
# churn (#2721). (issue #4277)
#
# What: Builds+installs trusty-assistants from local source (`cargo install
# --path crates/trusty-assistants --locked`), then codesigns the installed `ta`
# binary (`--identifier com.trusty.ta`) and its one-release `tagent` alias
# (`com.trusty.tagent`) by shelling out to `tctl sign trusty-assistants` — the
# single source of truth for trusty-* codesign flags/identifiers
# (`crates/trusty-installer/src/commands/macos_signing/mod.rs`, #2558). This
# script duplicates NO codesign logic, mirroring
# `install-trusty-search-signed.sh`'s `resolve_tctl()` delegation pattern.
#
# #11632: renamed from install-trusty-agents-signed.sh with the crate; the
# binary is `ta`, and `tagent` is kept for one release.
#
# Fail-soft requirement (THE key deviation from both sibling scripts, which
# hard-fail under `set -euo pipefail` when signing fails with no cert
# present): this script must NOT abort the whole run just because no
# Developer ID cert is present.
#   1. `cargo install --path crates/trusty-assistants --locked` always runs
#      and, if IT fails, that is a real build failure — the script exits
#      non-zero.
#   2. The signing step (`tctl sign trusty-assistants`) is wrapped so a
#      non-zero exit (no cert, or any other codesign failure) becomes a clear
#      stderr warning and the script continues, exiting 0 overall.
#
# Test: `bash -n scripts/install-trusty-assistants-signed.sh` for syntax check.
# `shellcheck scripts/install-trusty-assistants-signed.sh` for lint.
# `--dry-run` (or `TRUSTY_CODESIGN_DRY_RUN=1`) prints every cargo/tctl/codesign
# command WITHOUT executing it. Run on a machine without a Developer ID cert
# (or with a deliberately bogus `TRUSTY_SIGN_IDENTITY`) to validate the
# fail-soft warning path and confirm the script still exits 0.
#
# Usage:
#   # Standard: build from repo source (default) and sign ta and tagent
#   scripts/install-trusty-assistants-signed.sh
#
#   # Dry-run: print the commands without installing or signing anything
#   scripts/install-trusty-assistants-signed.sh --dry-run
#
#   # Override signing identity (e.g. multiple Developer ID certs)
#   TRUSTY_SIGN_IDENTITY="Developer ID Application: Acme Corp (ABCDE12345)" \
#     scripts/install-trusty-assistants-signed.sh
#
#   # Override cargo source path (defaults to the repo this script lives in)
#   TRUSTY_INSTALL_PATH=/path/to/trusty-tools scripts/install-trusty-assistants-signed.sh


set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration (all overridable via environment)
# ---------------------------------------------------------------------------

CARGO_BIN_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"
readonly TA_BIN="$CARGO_BIN_DIR/ta"
readonly TA_IDENTIFIER="com.trusty.ta"

# Sign identity: auto-detected by `tctl sign` if not overridden.
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
info()    { printf '\033[0;32m[install-assistants-signed] %s\033[0m\n' "$*" >&2; }
warn()    { printf '\033[0;33m[install-assistants-signed] WARNING: %s\033[0m\n' "$*" >&2; }
error()   { printf '\033[0;31m[install-assistants-signed] ERROR: %s\033[0m\n' "$*" >&2; }
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

# ---------------------------------------------------------------------------
# Step 1: cargo install (hard requirement — failure here is a real build
# failure, not the fail-soft signing case this script exists for)
# ---------------------------------------------------------------------------

run_cargo_install() {
    section "Step 1: cargo install trusty-assistants (from local source)"

    local repo_root crate_path
    if [[ -n "$TRUSTY_INSTALL_PATH" ]]; then
        repo_root="$TRUSTY_INSTALL_PATH"
    else
        repo_root="$(detect_repo_root)"
    fi
    crate_path="$repo_root/crates/trusty-assistants"

    if [[ "$TRUSTY_CODESIGN_DRY_RUN" == "1" ]]; then
        info "[DRY RUN] cargo install --path $crate_path --locked"
        return 0
    fi

    info "Installing trusty-assistants from: $crate_path"
    cargo install --path "$crate_path" --locked

    if [[ -f "$TA_BIN" ]]; then
        info "cargo install complete: $TA_BIN"
    else
        warn "$TA_BIN not found after cargo install (unexpected)"
    fi
}

# ---------------------------------------------------------------------------
# Step 2: sign ta and tagent (delegates to `tctl sign trusty-assistants` — #2558/#4277)
# fail-soft: a signing failure is a WARNING, not a script-aborting error.
# ---------------------------------------------------------------------------

run_sign_ta() {
    section "Step 2: Codesign ta and tagent via 'tctl sign trusty-assistants'"

    if [[ "$TRUSTY_CODESIGN_DRY_RUN" == "1" ]]; then
        info "[DRY RUN] Would run: tctl sign trusty-assistants --dir $CARGO_BIN_DIR (identifier=$TA_IDENTIFIER)"
        return 0
    fi

    local tctl_bin repo_root rc=0
    if tctl_bin="$(resolve_tctl)"; then
        info "Using $tctl_bin on PATH"
        "$tctl_bin" sign trusty-assistants --dir "$CARGO_BIN_DIR" || rc=$?
    else
        repo_root="$(detect_repo_root)"
        warn "tctl/trusty-installer not found on PATH — running via 'cargo run -p trusty-installer'"
        warn "(install trusty-installer once — cargo install --path $repo_root/crates/trusty-installer --locked — to skip this rebuild next time)"
        (cd "$repo_root" && cargo run --quiet -p trusty-installer -- sign trusty-assistants --dir "$CARGO_BIN_DIR") || rc=$?
    fi

    if [[ "$rc" -ne 0 ]]; then
        warn "Signing ta failed (exit $rc) — ta and tagent remain ad-hoc signed."
        warn "macOS will re-prompt for any TCC-gated permission on next launch — this"
        warn "is EXPECTED on CI or any machine without a Developer ID Application cert."
        warn "See docs/reference/release-workflow.md for one-time cert setup."
        return 0
    fi
    info "ta signed successfully (identifier=$TA_IDENTIFIER); tagent as com.trusty.tagent."
}

# ---------------------------------------------------------------------------
# Post-install guidance
# ---------------------------------------------------------------------------

print_next_steps() {
    section "Done — next steps"
    # shellcheck disable=SC2016  # literal "$HOME"/".trusty-assistants/" is narration, not expansion
    printf '\nta reads only project-local ".trusty-agents/" and\n'
    # shellcheck disable=SC2016
    printf '"$HOME/.trusty-assistants/" config/state (no external volumes) — analogous to\n'
    printf "trusty-mpm's App-Data category, no Full Disk Access re-grant should be\n"
    printf 'needed. If macOS prompts for data access from other apps (e.g. reading a\n'
    printf 'project-local .claude/ config dir), approve it once — the stable\n'
    printf 'Developer ID identity makes that approval persist across every future\n'
    printf 'cargo install.\n\n'
    printf 'Restart any running ta process to pick up the newly signed binary.\n'
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

    printf '\033[1m=== trusty-assistants signed install (fixes #4277) ===\033[0m\n\n' >&2
    if [[ "$TRUSTY_CODESIGN_DRY_RUN" == "1" ]]; then
        warn "DRY RUN — no cargo install, no signing."
    fi

    if [[ "$(uname)" != "Darwin" ]]; then
        error "This script is macOS-only (codesign / TCC are Apple-specific)."
        exit 1
    fi

    run_cargo_install
    run_sign_ta
    print_next_steps
}

main "$@"
