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
# UNRESOLVED, and deliberately not overclaimed below: macOS attributes
# Files-and-Folders access to the RESPONSIBLE process, which for a
# shell-invoked CLI is usually the terminal rather than the binary — and the
# setup/migrate path above is exactly that case. If the attribution lands on
# the terminal, signing trusty-memory does not change that path's prompting.
# The launchd-started daemon IS attributed to the binary, so a stable identity
# applies there. Nothing here was validated by a real signing run; this script
# states what signing does (a stable designated requirement — the necessary
# precondition), never that a particular grant will survive.
#
# What: Builds+installs trusty-memory from local source (`cargo install
# --path crates/trusty-memory --locked`), then codesigns BOTH installed
# binaries — `trusty-memory` (`com.trusty.trusty-memory`) and the deprecated
# `trusty-memory-mcp-bridge` shim (`com.trusty.trusty-memory-mcp-bridge`).
# #5329 removed the third, `trusty-bm25-daemon`: BM25 runs in-process now, so
# `cargo install trusty-memory` no longer produces that binary —
# by shelling out to `tctl sign trusty-memory`, the single source of truth for
# trusty-* codesign flags and identifiers
# (`crates/trusty-installer/src/commands/macos_signing/mod.rs`, #2558). This
# script duplicates NO codesign logic, mirroring
# `install-trusty-assistants-signed.sh`'s `resolve_tctl()` delegation exactly.
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
# Fail-soft requirement (matching install-trusty-assistants-signed.sh, not the
# hard-failing mpm/search scripts): a missing Developer ID certificate must
# not abort the run.
#   1. `cargo install --path crates/trusty-memory --locked` always runs and,
#      if IT fails, that is a real build failure — the script exits non-zero.
#   2. #11683: the signing step (`tctl sign trusty-memory`) soft-passes ONLY
#      its no-certificate exit (3): a stderr warning, the run ends with the
#      line `NOT SIGNED` as its last stdout line, and the script exits 0.
#      Any other non-zero sign exit (a codesign error, a locked keychain, a
#      signature that reads back ad-hoc or with the wrong Team ID, a failed
#      record refresh) prints an ERROR line and exits 1. The soft-pass is
#      pending an owner decision; flipping `NO_CERT_SOFT_PASS` to 0 below
#      makes it fail too.
#   3. #11683: a signer that cannot be built (no usable tctl on PATH and the
#      checkout's `tctl` fails to build) exits 1 — that is a broken install.
#
# Test: scripts/install_signed_tctl_fallback_selftest.sh (stub cargo and tctl).
# `bash -n scripts/install-trusty-memory-signed.sh` for syntax check.
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
readonly BRIDGE_BIN="$CARGO_BIN_DIR/trusty-memory-mcp-bridge"

# The CODESIGN identifiers `tctl sign trusty-memory` applies, mirrored here only
# to narrate the --dry-run plan. These are not launchd labels: they live in the
# codesign namespace, keep the full binary name on purpose, and must never be
# normalised onto `com.trusty.<stem>` — renaming one invalidates the binary's
# designated requirement and re-triggers macOS TCC prompts (#2558). The
# `*_IDENTIFIER=` naming is what marks them as codesign identifiers, matching
# install-trusty-mpm-signed.sh and install-trusty-assistants-signed.sh.
# #4868: the previous inline `-> com.trusty.…` narration carried no such marker,
# so the drift scan read them as unowned launchd labels.
readonly MEMORY_IDENTIFIER="com.trusty.trusty-memory"
readonly BRIDGE_IDENTIFIER="com.trusty.trusty-memory-mcp-bridge"

# Sign identity: auto-detected by `tctl sign` if not overridden. Exported so
# the child `tctl` process honours the same override this script was given.
TRUSTY_SIGN_IDENTITY="${TRUSTY_SIGN_IDENTITY:-}"
export TRUSTY_SIGN_IDENTITY

# Install source: path dep (default) — derived from this script's own location.
TRUSTY_INSTALL_PATH="${TRUSTY_INSTALL_PATH:-}"

# Dry-run mode: print commands without executing. Set via env or --dry-run flag.
TRUSTY_CODESIGN_DRY_RUN="${TRUSTY_CODESIGN_DRY_RUN:-0}"

# #11683: `tctl sign` exits 3 (`EXIT_NO_CERTIFICATE`) when no Developer ID
# certificate is available; only that exit soft-passes. Set to 0 to make it a
# hard failure like every other sign exit (pending an owner decision).
readonly TCTL_SIGN_NO_CERT_RC=3
readonly NO_CERT_SOFT_PASS=1
# #11683: set by the no-cert soft-pass; `main` then ends with `NOT SIGNED`.
NOT_SIGNED=0

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
# an already-installed `tctl`/`trusty-installer` on PATH whose `sign` accepts
# SET; falls back to building and running this checkout's `tctl` so a fresh
# checkout with no prior tctl install still works. Mirrors
# `install-trusty-assistants-signed.sh`'s `resolve_tctl()`.
# #11683: a tctl on PATH whose `sign` does not list SET is stale; say so and
# skip it rather than let it fail the sign step with a clap usage error.
# Test: scripts/install_signed_tctl_fallback_selftest.sh.
resolve_tctl() {
    local set="$1" candidate help values
    for candidate in tctl trusty-installer; do
        command -v "$candidate" >/dev/null 2>&1 || continue
        # #11683: read SET only from clap's `[possible values: ...]` list for
        # <TARGET> (`-h` prints it; `--help` prints a long form without it).
        help="$("$candidate" sign -h 2>&1 || true)"
        values="$(printf '%s\n' "$help" | sed -n 's/^.*<TARGET>.*\[possible values: \([^]]*\)\].*$/\1/p')"
        case ", $values, " in
            *", $set, "*) echo "$candidate"; return 0 ;;
        esac
        warn "$candidate on PATH is stale: its 'sign' does not accept '$set'; reinstall trusty-installer to refresh it"
    done
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
    for bin in "$MEMORY_BIN" "$BRIDGE_BIN"; do
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
# #11683: only a missing certificate is a WARNING; any other failure exits 1.
# ---------------------------------------------------------------------------

run_sign() {
    section "Step 2: Codesign the trusty-memory set via 'tctl sign trusty-memory'"

    if [[ "$TRUSTY_CODESIGN_DRY_RUN" == "1" ]]; then
        info "[DRY RUN] Would run: tctl sign trusty-memory --dir $CARGO_BIN_DIR"
        info "[DRY RUN]   trusty-memory            -> $MEMORY_IDENTIFIER"
        info "[DRY RUN]   trusty-memory-mcp-bridge -> $BRIDGE_IDENTIFIER"
        return 0
    fi

    local tctl_bin repo_root rc=0
    if tctl_bin="$(resolve_tctl trusty-memory)"; then
        info "Using $tctl_bin on PATH"
        "$tctl_bin" sign trusty-memory --dir "$CARGO_BIN_DIR" || rc=$?
    else
        repo_root="$(detect_repo_root)"
        warn "no usable tctl/trusty-installer on PATH — running via 'cargo run -p trusty-installer --bin tctl'"
        warn "(install trusty-installer once — cargo install --path $repo_root/crates/trusty-installer --locked — to skip this rebuild next time)"
        # #11683: a signer that cannot be built is a broken install, not the
        # missing-certificate case fail-soft covers; `--bin` because the crate
        # has two binaries and cargo cannot pick one.
        if ! (cd "$repo_root" && cargo build --quiet -p trusty-installer --bin tctl); then
            error "could not build this checkout's tctl in $repo_root; nothing was signed"
            exit 1
        fi
        (cd "$repo_root" && cargo run --quiet -p trusty-installer --bin tctl -- sign trusty-memory --dir "$CARGO_BIN_DIR") || rc=$?
    fi

    # #11683: the one soft-pass — no Developer ID certificate.
    if [[ "$rc" -eq "$TCTL_SIGN_NO_CERT_RC" && "$NO_CERT_SOFT_PASS" == "1" ]]; then
        warn "Signing the trusty-memory set failed (exit $rc: no Developer ID certificate) — the binaries remain ad-hoc signed."
        warn "macOS will re-prompt for any TCC-gated permission after the next rebuild — this"
        warn "is EXPECTED on CI or any machine without a Developer ID Application cert."
        warn "See docs/reference/release-workflow.md for one-time cert setup."
        NOT_SIGNED=1
        return 0
    fi
    if [[ "$rc" -ne 0 ]]; then
        error "Signing the trusty-memory set failed (exit $rc); see the tctl output above."
        exit 1
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
    printf 'The category this addresses is Files-and-Folders, reached by\n'
    printf '"trusty-memory setup" / "trusty-memory migrate", which walk $HOME for\n'
    printf '.claude/settings*.json and so touch ~/Desktop, ~/Documents, and\n'
    printf '~/Downloads. Approve those prompts when they appear.\n\n'
    printf 'What signing changes, precisely: the binaries now carry a stable\n'
    printf 'designated requirement instead of a cdhash that changes on every\n'
    printf 'cargo install. That is what a durable TCC grant REQUIRES; it is not\n'
    printf 'on its own proof that one was obtained. Two caveats:\n\n'
    printf '  * macOS attributes Files-and-Folders access to the RESPONSIBLE\n'
    printf '    process. For a CLI you launched from a shell that is usually the\n'
    printf '    terminal app, not this binary — so on the setup/migrate path the\n'
    printf '    prompt may name your terminal and signing trusty-memory would not\n'
    printf '    change it. The launchd-started daemon is attributed to the binary\n'
    printf '    itself, where a stable identity does apply.\n'
    printf '  * Persistence across a reinstall has not been confirmed here by a\n'
    printf '    signing run. Verify on your own machine before relying on it.\n\n'
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
    # #11683 (Architect ruling on AC3): an unsigned run's last line, on stdout
    # with the other final status lines.
    if [[ "$NOT_SIGNED" == "1" ]]; then
        printf 'NOT SIGNED\n'
    fi
}

main "$@"
