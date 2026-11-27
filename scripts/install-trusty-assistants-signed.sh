#!/usr/bin/env bash
# scripts/install-trusty-assistants-signed.sh
#
# Why: Every `cargo install` of trusty-assistants produces an ad-hoc /
# linker-signed `ta` with a new cdhash. macOS TCC keys grants by cdhash, so
# any TCC category `ta` has triggered is revoked on every rebuild. Signing
# with a stable Developer ID identity and a fixed `--identifier` keeps the
# designated requirement stable across rebuilds (#4277, same class as #873
# and #2721).
#
# What: installs the `ta` binary ONLY, then signs it through
# `tctl sign trusty-assistants`, the single source of truth for codesign
# flags and identifiers (#2558). This script holds no codesign logic.
#
#   The `tagent` guard (#11650 slice 3d): the crate still builds a one-release
#   `tagent` alias, but this script never installs it. A launchd Slack bot
#   runs the `tagent` that is already in the install root, and that file
#   belongs to the older `trusty-agents` package.
#     - cargo always runs with `--bin ta`; `--bin <name>` and `tagent` are
#       refused as arguments.
#     - Signing runs on a staging directory that holds only `ta` (moved there
#       and back by rename). The `trusty-assistants` signing set lists `ta`
#       and `tagent`, so signing `<root>/bin` in place would re-sign the
#       bot's binary. A `tctl` on PATH whose
#       `sign` does not accept `trusty-assistants` (installed before #11632)
#       is skipped; this checkout's trusty-installer is built and signs.
#     - The script fingerprints `<root>/bin/tagent` (absent, or inode, size,
#       mtime and sha256) before it starts, and exits 1 if the fingerprint
#       differs afterwards.
#
#   Source, per ADR-0043 (no `--path` install into `$CARGO_HOME/bin`):
#     (default)       `cargo install trusty-assistants --version <v>` from
#                     crates.io; <v> is this checkout's crate version unless
#                     `--version` names one.
#     --git-rev SHA   `cargo install --git <repo> --rev SHA trusty-assistants`,
#                     a pinned, persistent source (how the live `tagent` was
#                     installed). Use a commit on merged main.
#     --from-source   `cargo install --path <checkout>/crates/trusty-assistants`.
#                     Refused unless `--root` names a directory other than
#                     the cargo home, so a path install only ever lands in a
#                     scratch root.
#
#   The cargo install (#11733) runs with git-CLI fetch, so a host whose `git`
#   authenticates but whose ssh-agent holds no identity can still fetch, and
#   with a new `--target-dir` removed afterwards, never an inherited
#   CARGO_TARGET_DIR, so no earlier rev's build-script output (its commit
#   stamp) is reused.
#
#   Fail-soft signing: a cargo failure exits non-zero; a signing failure (no
#   certificate, or any codesign error) is a warning and the run exits 0.
#   A signer that cannot be built (no usable tctl on PATH and this checkout's
#   `tctl` fails to build) exits 1 (#11683).
#
# Test: scripts/install_trusty_assistants_signed_selftest.sh (stub cargo and
# tctl; tagent untouched, refusals, staging, dry run).
# `shellcheck scripts/install-trusty-assistants-signed.sh` for lint.
#
# Usage:
#   scripts/install-trusty-assistants-signed.sh [--root DIR]
#       [--git-rev SHA | --from-source | --version V] [--dry-run]
#
#   # Real install from a merged commit (into $CARGO_HOME/bin):
#   scripts/install-trusty-assistants-signed.sh --git-rev <sha>
#
#   # Scratch install of this checkout into a temp root:
#   scripts/install-trusty-assistants-signed.sh --from-source --root "$(mktemp -d)"
#
#   # Print every command without running any:
#   scripts/install-trusty-assistants-signed.sh --git-rev <sha> --dry-run
#
#   TRUSTY_SIGN_IDENTITY=...   override the identity `tctl sign` selects
#   TRUSTY_CODESIGN_DRY_RUN=1  same as --dry-run
#
# Exit: 0 installed (signed or not); 1 install failed, signer could not be
# built, or `tagent` changed;
# 2 usage error or refused argument.

# #7812: re-exec under bash when started from zsh.
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

readonly CRATE="trusty-assistants"
readonly BIN_NAME="ta"
readonly GUARDED_BIN="tagent"
readonly TA_IDENTIFIER="com.trusty.ta"
readonly REPO_URL="https://github.com/bobmatnyc/trusty-tools"

TRUSTY_SIGN_IDENTITY="${TRUSTY_SIGN_IDENTITY:-}"
export TRUSTY_SIGN_IDENTITY
DRY_RUN="${TRUSTY_CODESIGN_DRY_RUN:-0}"

# Log helpers write to stderr (#2322).
info()    { printf '[install-assistants-signed] %s\n' "$*" >&2; }
warn()    { printf '[install-assistants-signed] WARNING: %s\n' "$*" >&2; }
error()   { printf '[install-assistants-signed] ERROR: %s\n' "$*" >&2; }
section() { printf '\n==> %s\n' "$*" >&2; }

usage_error() {
    error "$*"
    error "usage: $0 [--root DIR] [--git-rev SHA | --from-source | --version V] [--dry-run]"
    exit 2
}

refuse_tagent() {
    error "refused: this script installs only \`$BIN_NAME\`. \`$GUARDED_BIN\` is never installed:"
    error "the launchd Slack bot runs the existing \`$GUARDED_BIN\`, which belongs to trusty-agents."
    exit 2
}

repo_root() {
    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
    dirname "$script_dir"
}

# The `version = "..."` of this checkout's crate manifest.
checkout_version() {
    sed -n 's/^version = "\(.*\)"$/\1/p' "$(repo_root)/crates/$CRATE/Cargo.toml" | head -n 1
}

# Physical path of an existing directory; empty when it does not exist.
physical() {
    (cd "$1" 2>/dev/null && pwd -P) || true
}

# fingerprint PATH: `absent`, or inode, size, mtime and sha256 of the file.
# #11650: fails closed. errexit does not reach inside `$(...)`, so every step is
# checked by hand; a missing field would make before == after and hide a change.
fingerprint() {
    local path="$1" meta sum
    if [ ! -e "$path" ] && [ ! -L "$path" ]; then
        echo "absent"
        return 0
    fi
    case "$(uname -s)" in
        Darwin|*BSD) meta="$(stat -f '%i %z %m' "$path")" || meta="" ;;
        *) meta="$(stat -c '%i %s %Y' "$path")" || meta="" ;;
    esac
    sum="$(shasum -a 256 "$path")" || sum=""
    sum="${sum%% *}"
    if [ -z "$meta" ] || [ -z "$sum" ]; then
        error "cannot fingerprint $path (stat or sha256 failed); refusing to continue"
        return 1
    fi
    echo "$meta $sum"
}

# A `tctl`/`trusty-installer` on PATH whose `sign` accepts this crate's set;
# fails when none does. A tctl older than #11632 knows only `trusty-agents`.
resolve_tctl() {
    local candidate
    for candidate in tctl trusty-installer; do
        command -v "$candidate" >/dev/null 2>&1 || continue
        if "$candidate" sign --help 2>&1 | grep -q -- "$CRATE"; then
            echo "$candidate"
            return 0
        fi
        warn "$candidate on PATH does not accept 'sign $CRATE' (installed before #11632)"
    done
    return 1
}

ROOT=""
SOURCE="registry"
GIT_REV=""
VERSION=""

parse_args() {
    local picked=0
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --dry-run) DRY_RUN=1; shift ;;
            --root) [ "$#" -ge 2 ] || usage_error "--root needs a directory"; ROOT="$2"; shift 2 ;;
            --git-rev)
                [ "$#" -ge 2 ] || usage_error "--git-rev needs a commit"
                SOURCE="git"; GIT_REV="$2"; picked=$((picked + 1)); shift 2 ;;
            --from-source) SOURCE="path"; picked=$((picked + 1)); shift ;;
            --version)
                [ "$#" -ge 2 ] || usage_error "--version needs a version"
                SOURCE="registry"; VERSION="$2"; picked=$((picked + 1)); shift 2 ;;
            --bin|--bins|--bin=*|"$GUARDED_BIN") refuse_tagent ;;
            -h|--help)
                awk 'NR==1{next} /^#/{sub(/^# ?/,""); print; next} {exit}' "${BASH_SOURCE[0]}" >&2
                exit 0 ;;
            *) usage_error "unknown argument: $1" ;;
        esac
    done
    [ "$picked" -le 1 ] || usage_error "pick one source: --git-rev, --from-source or --version"
    case "$GIT_REV" in *[!0-9a-f]*) usage_error "--git-rev takes a hex commit sha: $GIT_REV" ;; esac
}

CARGO_HOME_DIR="${CARGO_HOME:-$HOME/.cargo}"

resolve_root() {
    local made_for_dry_run=0
    if [ -z "$ROOT" ]; then
        ROOT="$CARGO_HOME_DIR"
    fi
    case "$ROOT" in /*) ;; *) usage_error "--root must be an absolute path: $ROOT" ;; esac
    if [ ! -d "$ROOT" ]; then
        if [ "$DRY_RUN" = "1" ]; then made_for_dry_run=1; else mkdir -p "$ROOT"; fi
    fi
    if [ "$made_for_dry_run" = "0" ]; then
        ROOT="$(physical "$ROOT")"
    fi
    BIN_DIR="$ROOT/bin"

    if [ "$SOURCE" = "path" ]; then
        local cargo_home
        cargo_home="$(physical "$CARGO_HOME_DIR")"
        [ -n "$cargo_home" ] || cargo_home="$CARGO_HOME_DIR"
        if [ "$ROOT" = "$cargo_home" ]; then
            error "refused: --from-source is a path install, and ADR-0043 keeps path installs"
            error "out of $cargo_home/bin. Use --git-rev <merged sha>, or --root <scratch dir>."
            exit 2
        fi
    fi
}

# install_argv TARGET_DIR: the cargo install command, into INSTALL.
install_argv() {
    # #11733: git-CLI fetch for this command only; a fresh target dir, not the
    # caller's CARGO_TARGET_DIR, so a previous rev's build.rs output is not reused.
    INSTALL=(env -u CARGO_TARGET_DIR CARGO_NET_GIT_FETCH_WITH_CLI=true cargo install)
    case "$SOURCE" in
        registry) INSTALL+=("$CRATE" --version "${VERSION:-$(checkout_version)}") ;;
        git) INSTALL+=(--git "$REPO_URL" --rev "$GIT_REV" "$CRATE") ;;
        path) INSTALL+=(--path "$(repo_root)/crates/$CRATE") ;;
    esac
    INSTALL+=(--bin "$BIN_NAME" --locked --root "$ROOT" --target-dir "$1")
}

TARGET_DIR=""
# Remove the install's scratch target dir; safe to call twice or when unset.
# shellcheck disable=SC2329  # invoked through the trap in main
cleanup_target() {
    if [ -n "$TARGET_DIR" ]; then rm -rf "$TARGET_DIR"; fi
    TARGET_DIR=""
}

# #11733: one EXIT handler for every scratch resource, so INT/TERM during
# `cargo install` cannot strand a build dir in $TMPDIR.
# shellcheck disable=SC2329  # invoked through the trap in main
cleanup_all() {
    cleanup_target
    restore_staged
}

run_cargo_install() {
    section "Step 1: install \`$BIN_NAME\` only (source: $SOURCE)"
    local tmp="${TMPDIR:-/tmp}" target rc=0
    tmp="${tmp%/}"
    if [ "$DRY_RUN" = "1" ]; then
        install_argv "<$tmp/ta-install-target.XXXXXX>"
        info "[DRY RUN] ${INSTALL[*]}"
        return 0
    fi
    target="$(mktemp -d "$tmp/ta-install-target.XXXXXX")"
    TARGET_DIR="$target"
    install_argv "$target"
    info "${INSTALL[*]}"
    "${INSTALL[@]}" || rc=$?
    cleanup_target
    [ "$rc" -eq 0 ] || exit "$rc"
    [ -f "$BIN_DIR/$BIN_NAME" ] || { error "$BIN_DIR/$BIN_NAME missing after cargo install"; exit 1; }
    info "installed $BIN_DIR/$BIN_NAME"
}

STAGE=""
# Put `ta` back if the run stops while it sits in the staging directory.
# shellcheck disable=SC2329  # invoked through the trap in main
restore_staged() {
    if [ -n "$STAGE" ] && [ -f "$STAGE/$BIN_NAME" ]; then
        mv "$STAGE/$BIN_NAME" "$BIN_DIR/$BIN_NAME"
    fi
    if [ -n "$STAGE" ]; then rmdir "$STAGE" 2>/dev/null || true; fi
    STAGE=""
}

run_sign_ta() {
    section "Step 2: codesign \`$BIN_NAME\` alone via 'tctl sign $CRATE'"
    if [ "$DRY_RUN" = "1" ]; then
        info "[DRY RUN] mv $BIN_DIR/$BIN_NAME <$ROOT/.ta-sign.XXXXXX>/$BIN_NAME"
        info "[DRY RUN] tctl sign $CRATE --dir <$ROOT/.ta-sign.XXXXXX> (identifier=$TA_IDENTIFIER)"
        info "[DRY RUN] mv <$ROOT/.ta-sign.XXXXXX>/$BIN_NAME $BIN_DIR/$BIN_NAME"
        return 0
    fi

    # Pick (and, for the fallback, build) the signer before `ta` leaves $BIN_DIR.
    local tctl_bin="" rc=0
    if ! tctl_bin="$(resolve_tctl)"; then
        tctl_bin=""
        warn "no usable tctl on PATH; building this checkout's trusty-installer"
        # #11683: a signer that cannot be built is a broken install, not the
        # missing-certificate case fail-soft covers.
        if ! (cd "$(repo_root)" && cargo build --quiet -p trusty-installer --bin tctl); then
            error "could not build this checkout's tctl in $(repo_root); $BIN_NAME was not signed"
            exit 1
        fi
    fi

    if [ "$rc" -eq 0 ]; then
        # Same filesystem as $BIN_DIR, so both moves are renames, never copies.
        STAGE="$(mktemp -d "$ROOT/.ta-sign.XXXXXX")"
        mv "$BIN_DIR/$BIN_NAME" "$STAGE/$BIN_NAME"
        info "staging dir holds only $BIN_NAME: $STAGE"
        if [ -n "$tctl_bin" ]; then
            info "signing with $tctl_bin on PATH"
            "$tctl_bin" sign "$CRATE" --dir "$STAGE" || rc=$?
        else
            info "signing with 'cargo run -p trusty-installer' from $(repo_root)"
            (cd "$(repo_root)" && cargo run --quiet -p trusty-installer --bin tctl -- sign "$CRATE" --dir "$STAGE") || rc=$?
        fi
        restore_staged
    fi

    if [ "$rc" -ne 0 ]; then
        warn "signing $BIN_NAME failed (exit $rc); it stays ad-hoc signed and macOS will"
        warn "re-prompt for TCC-gated access after each rebuild. Expected on a machine"
        warn "without a Developer ID Application cert: docs/reference/release-workflow.md."
        return 0
    fi
    info "$BIN_NAME signed (identifier=$TA_IDENTIFIER)"
}

main() {
    parse_args "$@"
    printf '=== trusty-assistants signed install: %s only (#4277, #11650) ===\n' "$BIN_NAME" >&2
    if [ "$DRY_RUN" = "1" ]; then warn "DRY RUN: nothing is installed, moved or signed."; fi

    if [ "$(uname -s)" != "Darwin" ]; then
        error "This script is macOS-only (codesign / TCC are Apple-specific)."
        exit 1
    fi

    resolve_root
    local guarded="$BIN_DIR/$GUARDED_BIN" before after
    before="$(fingerprint "$guarded")" || exit 1
    info "$GUARDED_BIN guard: $guarded is $before"

    # INT/TERM exit through the EXIT trap (bash runs it after the foreground child).
    trap cleanup_all EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    run_cargo_install
    run_sign_ta
    trap - EXIT INT TERM

    after="$(fingerprint "$guarded")" || exit 1
    if [ "$after" != "$before" ]; then
        error "$guarded changed during the install: before [$before], after [$after]"
        exit 1
    fi
    info "$GUARDED_BIN guard: $guarded unchanged ($after)"
    info "Restart any running $BIN_NAME process to pick up the new binary."
}

main "$@"
