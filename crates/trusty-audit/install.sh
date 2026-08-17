#!/bin/sh
# install.sh — self-downloading macOS installer for `trusty-audit`.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/crates/trusty-audit/install.sh | sh
#
# Why (#5870): the owner's requirement is "enter a URL, trusty-audit installs AND
#   runs". Before this, the only delivery path was `trusty-audit distribute`
#   (#5825) — an operator builds a zip and emails it. That does not survive
#   contact with a client site: the recipient has no Rust toolchain, no
#   checkout, and no reason to trust a zip attachment.
#
# What: detects the platform, resolves a release, downloads the tarball and its
#   published `.sha256` sidecar, verifies the digest, extracts to a temp dir,
#   proves the binary reports the version that was asked for, installs it with
#   an atomic rename, and launches it. Everything past that point — provider
#   reachability, the credential, the collection tools — is trusty-audit's own.
#
# Test: `scripts/check_trusty_audit_install_selftest.sh` drives every arm against
#   stubbed `curl`/`uname` with no network — non-Darwin uname, Intel uname,
#   checksum mismatch, failed download, happy path, and idempotent re-run.
#   `sh -n` and `shellcheck --shell=sh` gate syntax and lint. All three run in
#   `.github/workflows/trusty-audit-install.yml`.
#
# ── This is a BOOTSTRAP. trusty-audit is the installer. ─────────────────────
# trusty-audit is an installer/collector/auditor: `src/tools.rs` already
# installs its own pinned tooling (`tga`, `trusty-search`, `trusty-analyze`,
# `trusty-review`) through `trusty_installer::download::pinned::
# install_pinned_set`, then collects, then audits. This script's ONLY job is
# getting the `trusty-audit` binary onto the machine and launching it.
#
# So the checks here are deliberately few, and each one gates whether the
# binary can run AT ALL — which is the only thing a shell script is better
# placed to answer than the binary is:
#
#   1. the host is a supported macOS architecture (a binary for another
#      platform cannot be exec'd, so nothing downstream could report this),
#   2. the tools THIS SCRIPT uses to do its own job exist,
#   3. the download matched its published checksum,
#   4. the downloaded binary actually executes and reports a version.
#
# Everything else is trusty-audit's to check, at the point it needs it: whether the
# inference provider is reachable, whether the credential works, and later the
# COLLECTION dependencies (`gh`, JIRA, Linear) at the point the operator names
# a repository and a ticketing system. Re-implementing any of those here would
# be a second implementation of logic the binary already owns in Rust — with
# real error types, its own tests, and sharing with the TUI and Tauri front
# ends — which CLAUDE.md's common-entry-point rule treats as a defect.
# `crates/trusty-audit/src/discover.rs` is where the `gh` dependency actually
# becomes real.
#
# ── Deliberate omission of `pipefail` ────────────────────────────────────────
# `set -o pipefail` is NOT POSIX and this script is invoked as `curl … | sh`,
# where `sh` is whatever the host provides. Rather than depend on it, no
# pipeline anywhere below is load-carrying: every command whose failure matters
# is run on its own and its status checked directly. `set -eu` is set.
#
# Environment variables:
#   TRUSTY_AUDIT_VERSION            Pin an exact version (e.g. "0.1.0"). Default: latest.
#   TRUSTY_AUDIT_INSTALL_DIR        Install dir. Default: ${CARGO_HOME:-$HOME/.cargo}/bin
#   TRUSTY_AUDIT_NO_LAUNCH          Set to 1 to install without launching.
#   GITHUB_TOKEN / GH_TOKEN   Optional. Raises the GitHub API rate limit from
#                             60/hr (unauthenticated, easily exhausted behind a
#                             shared/NAT'd IP) to 5000/hr. Never required.

set -eu

# ---------------------------------------------------------------------------
# Constants. Every magic string lives here, not scattered through the script.
# These URL shapes are the SAME ones the Rust installer builds in
# `crates/trusty-installer/src/download/release.rs` (`asset_url` /
# `sha256_url`) and that `.github/workflows/release.yml` publishes. They are
# one convention with three implementations, so any change to the asset naming
# has to land in all three.
# ---------------------------------------------------------------------------
REPO="bobmatnyc/trusty-tools"
CRATE="trusty-audit"
PRIMARY_BIN="trusty-audit"
ALIAS_BIN="taudit"
TAG_PREFIX="${CRATE}-v"

API_RELEASES_URL="https://api.github.com/repos/${REPO}/releases"
RELEASE_DL_BASE="https://github.com/${REPO}/releases/download"

# The one supported target. `docs/distribution/INSTALL-CONVENTION.md` records
# the workspace-wide decision: "Not supported: macOS x86_64 (Intel) — only
# Apple Silicon (aarch64-apple-darwin) is targeted". No x86_64-apple-darwin
# asset is built by the release workflow for ANY crate in this workspace, so an
# Intel Mac is refused below rather than handed an arm64 binary it cannot exec.
TARGET="aarch64-apple-darwin"

# Default install dir — the canonical cargo bin dir, matching the root
# `install.sh` and every other write path in this workspace (#5777 / #4964: two
# destinations meant PATH order decided which copy ran). It needs no `sudo`,
# exists or is creatable on a stock Mac, and is already on PATH for anyone who
# has used a Rust tool. No Rust toolchain is required to USE it — this is pure
# path arithmetic and the script writes the binary there itself.
DEFAULT_INSTALL_DIR="${CARGO_HOME:-${HOME}/.cargo}/bin"

# Network timeouts, in seconds. Every network call names its own timeout in the
# failure message so an operator behind a slow proxy knows what was waited on.
CONNECT_TIMEOUT=10
API_MAX_TIME=30
DOWNLOAD_MAX_TIME=300

# Populated by main; declared here so the cleanup trap can never reference an
# unset variable under `set -u`.
STAGING_DIR=""

# ---------------------------------------------------------------------------
# Output helpers. Everything diagnostic goes to stderr so that stdout stays
# usable if a caller ever pipes this script's output.
# ---------------------------------------------------------------------------
say() { printf '%s\n' "$*" >&2; }
step() { printf '==> %s\n' "$*" >&2; }
ok() { printf '    ok: %s\n' "$*" >&2; }

# Fail with a message that names WHAT failed, WHY, and WHAT TO DO. An operator
# at a client site with no context has only this text to act on, so every call
# site below supplies all three parts.
die() {
    printf '\nERROR: %s\n\n' "$1" >&2
    exit 1
}

cleanup() {
    if [ -n "${STAGING_DIR}" ] && [ -d "${STAGING_DIR}" ]; then
        rm -rf "${STAGING_DIR}"
    fi
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------------------
# Check 1 — the tools this script itself needs.
#
# Why: every one of these ships with a stock macOS, so a miss means a
# deliberately stripped or badly-PATH'd environment. Naming the missing tool is
# far more useful than the "command not found" that would otherwise surface
# from somewhere in the middle of a download.
# ---------------------------------------------------------------------------
require_host_tools() {
    step "Checking host tools"
    missing=""
    for tool in curl tar mktemp uname chmod mv; do
        if ! command -v "${tool}" >/dev/null 2>&1; then
            missing="${missing} ${tool}"
        fi
    done
    if [ -n "${missing}" ]; then
        die "Required tool(s) not found on PATH:${missing}
These ship with macOS, so PATH is probably restricted or the tools were removed.
What to do: run 'echo \$PATH' and confirm /usr/bin and /bin are present."
    fi

    # Checksum tool: macOS ships `shasum`; `sha256sum` exists if coreutils is
    # installed. Either satisfies the verification step.
    if command -v shasum >/dev/null 2>&1; then
        SHA_CMD="shasum -a 256"
    elif command -v sha256sum >/dev/null 2>&1; then
        SHA_CMD="sha256sum"
    else
        die "No SHA-256 tool found (looked for 'shasum' and 'sha256sum').
Without one the download cannot be verified, and this installer will not place
an unverified binary on your PATH.
What to do: confirm /usr/bin/shasum exists; it ships with macOS."
    fi
    ok "curl, tar, ${SHA_CMD% *} present"
}

# ---------------------------------------------------------------------------
# Check 2 — platform.
#
# Why: an arm64 Mach-O binary cannot execute on an Intel Mac at all (Rosetta 2
# translates x86_64 -> arm64, never the reverse), and a Linux host cannot run a
# Mach-O binary in any form. Both are refused HERE, before any network call, so
# an unsupported host never downloads something it cannot run.
# ---------------------------------------------------------------------------
check_platform() {
    step "Checking platform"
    os="$(uname -s)"
    arch="$(uname -m)"

    if [ "${os}" != "Darwin" ]; then
        die "Unsupported operating system: ${os} (this installer supports macOS only).
trusty-audit ships as a macOS binary; there is no ${os} asset to download.
What to do: run this on a Mac, or build from source with
  cargo install --path crates/${CRATE} --locked"
    fi

    if [ "${arch}" != "arm64" ]; then
        die "Unsupported macOS architecture: ${arch} (Apple Silicon / arm64 required).
No x86_64 (Intel) macOS asset is published for any crate in this workspace —
see docs/distribution/INSTALL-CONVENTION.md, 'Not supported: macOS x86_64'.
Downloading the arm64 binary here would give you a file that cannot execute.
What to do: run this on an Apple Silicon Mac, or build from source with
  cargo install --path crates/${CRATE} --locked"
    fi
    ok "macOS ${arch} -> ${TARGET}"
}

# ---------------------------------------------------------------------------
# Resolve which version to install.
#
# Why: an operator who was handed a URL wants the current release; an operator
# reproducing an engagement needs an exact pin. Both are explicit — there is no
# "whatever happens to be there" path that reports success either way.
# ---------------------------------------------------------------------------
resolve_version() {
    if [ -n "${TRUSTY_AUDIT_VERSION:-}" ]; then
        VERSION="${TRUSTY_AUDIT_VERSION}"
        step "Using pinned version ${VERSION} (TRUSTY_AUDIT_VERSION)"
        return 0
    fi

    step "Resolving latest ${CRATE} release"
    api_out="${STAGING_DIR}/releases.json"

    # No pipeline here: the download and the parse are separate so a curl
    # failure is caught on its own rather than masked by a successful grep.
    set +e
    if [ -n "${GITHUB_TOKEN:-${GH_TOKEN:-}}" ]; then
        curl -fsSL \
            --connect-timeout "${CONNECT_TIMEOUT}" --max-time "${API_MAX_TIME}" \
            -H "Authorization: Bearer ${GITHUB_TOKEN:-${GH_TOKEN:-}}" \
            -o "${api_out}" "${API_RELEASES_URL}?per_page=100"
    else
        curl -fsSL \
            --connect-timeout "${CONNECT_TIMEOUT}" --max-time "${API_MAX_TIME}" \
            -o "${api_out}" "${API_RELEASES_URL}?per_page=100"
    fi
    curl_status=$?
    set -e

    if [ "${curl_status}" -ne 0 ]; then
        die "Could not reach the GitHub releases API (curl exit ${curl_status}).
Timeouts used: ${CONNECT_TIMEOUT}s to connect, ${API_MAX_TIME}s total.
URL: ${API_RELEASES_URL}
What to do: check network/proxy access to api.github.com. If you are rate
limited (60 requests/hour unauthenticated), set GITHUB_TOKEN and re-run, or
pin a version with TRUSTY_AUDIT_VERSION=<x.y.z> to skip this lookup entirely."
    fi

    # Extract the highest-sorting `trusty-audit-v*` tag. grep/sed only — no jq
    # dependency, matching the root install.sh approach.
    VERSION="$(
        tr ',' '\n' <"${api_out}" |
            sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"'"${TAG_PREFIX}"'\([0-9][^"]*\)".*/\1/p' |
            sort -t. -k1,1n -k2,2n -k3,3n |
            tail -1
    )"

    if [ -z "${VERSION}" ]; then
        die "No published ${TAG_PREFIX}* release found in the GitHub releases API.
This means no ${CRATE} binary has been released yet, so there is nothing to
install. The release is produced by tagging ${TAG_PREFIX}<version>, which drives
.github/workflows/release.yml.
What to do: ask for a released version, or build from source with
  cargo install --path crates/${CRATE} --locked"
    fi
    ok "latest is ${VERSION}"
}

# ---------------------------------------------------------------------------
# Download and verify.
#
# Why: a `curl | sh` installer that executes an unverified binary is exactly
# the risk this crate exists to be careful about. Nothing is placed on PATH
# before the digest matches.
#
# What this verification DOES protect against: a truncated or corrupted
# transfer, a cache or mirror serving stale bytes, and an asset swapped after
# publication without the sidecar being regenerated.
#
# What it does NOT protect against: a compromised release pipeline. The
# `.sha256` sidecar is published by the same workflow, to the same host, as the
# tarball — an attacker who can replace one can replace the other. HTTPS to
# github.com is what actually authenticates the origin here. This matches the
# posture already documented in `crates/trusty-installer/src/download/
# pinned.rs`. An independent gate would need a signature over a key not held by
# the pipeline; that does not exist yet for this crate.
# ---------------------------------------------------------------------------
download_and_verify() {
    tag="${TAG_PREFIX}${VERSION}"
    asset="${CRATE}-${VERSION}-${TARGET}.tar.gz"
    asset_url="${RELEASE_DL_BASE}/${tag}/${asset}"
    sha_url="${asset_url}.sha256"

    TARBALL="${STAGING_DIR}/${asset}"
    sha_file="${TARBALL}.sha256"

    step "Downloading checksum sidecar"
    set +e
    curl -fsSL --connect-timeout "${CONNECT_TIMEOUT}" --max-time "${API_MAX_TIME}" \
        -o "${sha_file}" "${sha_url}"
    sha_status=$?
    set -e
    if [ "${sha_status}" -ne 0 ]; then
        die "Could not download the checksum sidecar (curl exit ${sha_status}).
Timeouts used: ${CONNECT_TIMEOUT}s to connect, ${API_MAX_TIME}s total.
URL: ${sha_url}
A missing sidecar means version ${VERSION} may not publish a ${TARGET} asset.
What to do: confirm the version exists at
  https://github.com/${REPO}/releases/tag/${tag}
Nothing has been installed."
    fi
    ok "sidecar downloaded"

    step "Downloading ${asset}"
    set +e
    curl -fsSL --connect-timeout "${CONNECT_TIMEOUT}" --max-time "${DOWNLOAD_MAX_TIME}" \
        -o "${TARBALL}" "${asset_url}"
    dl_status=$?
    set -e
    if [ "${dl_status}" -ne 0 ]; then
        die "Could not download the release archive (curl exit ${dl_status}).
Timeouts used: ${CONNECT_TIMEOUT}s to connect, ${DOWNLOAD_MAX_TIME}s total.
URL: ${asset_url}
What to do: check network/proxy access to github.com and retry. If the download
is simply slow, raise the ceiling by re-running with a longer allowance.
Nothing has been installed."
    fi
    ok "archive downloaded"

    step "Verifying SHA-256"
    # The sidecar is `<hex>  <filename>` (sha256sum / shasum -a 256 format).
    expected="$(awk '{print $1; exit}' "${sha_file}")"
    actual="$(${SHA_CMD} "${TARBALL}" | awk '{print $1; exit}')"

    if [ -z "${expected}" ]; then
        die "The checksum sidecar was empty or unparseable: ${sha_file}
Expected the format '<hex>  <filename>'.
Refusing to install an unverified binary. Nothing has been installed."
    fi

    if [ "${expected}" != "${actual}" ]; then
        die "CHECKSUM MISMATCH — refusing to install.
  expected: ${expected}
  actual:   ${actual}
  asset:    ${asset_url}
The downloaded file is not the published artifact. This is either a corrupted
transfer or a tampered download; either way it will not be placed on your PATH.
What to do: retry once. If it mismatches again, do not use the file — report it
against ${REPO}. Nothing has been installed."
    fi
    ok "sha256 ${actual}"
}

# ---------------------------------------------------------------------------
# Extract and prove the binary runs.
#
# Why: a checksum proves the bytes are the published bytes; it does not prove
# the published bytes are a working binary for this host. Executing
# `--version` in the staging dir catches a mis-tagged or mis-built asset before
# anything reaches PATH — the same reasoning, and the same accepted trade-off,
# recorded in `crates/trusty-installer/src/download/pinned.rs` check 5.
# ---------------------------------------------------------------------------
extract_and_prove() {
    step "Extracting"
    EXTRACT_DIR="${STAGING_DIR}/extract"
    mkdir -p "${EXTRACT_DIR}"
    if ! tar -xzf "${TARBALL}" -C "${EXTRACT_DIR}"; then
        die "Could not extract ${TARBALL}.
The archive downloaded and its checksum matched, so this is an unexpected
tar failure rather than a corrupt download.
What to do: retry. Nothing has been installed."
    fi

    STAGED_BIN="$(find "${EXTRACT_DIR}" -type f -name "${PRIMARY_BIN}" -perm -u+x | head -1)"
    if [ -z "${STAGED_BIN}" ]; then
        die "The archive did not contain a '${PRIMARY_BIN}' executable.
Archive: ${TARBALL}
This means the release asset was built without the expected binary target.
What to do: report it against ${REPO}. Nothing has been installed."
    fi
    ok "found ${PRIMARY_BIN}"

    # Gatekeeper / quarantine.
    #
    # MEASURED, not assumed (#5870): downloading a release tarball with `curl`
    # on macOS 15 (Darwin 25.5) sets `com.apple.provenance` and NOT
    # `com.apple.quarantine`, and the extracted binary executes with no
    # Gatekeeper prompt. `com.apple.quarantine` is applied by LaunchServices-
    # aware downloaders (browsers), which `curl` is not. The strip below is
    # therefore a no-op on the `curl | sh` path and exists only for the operator
    # who downloads this script or the tarball through a browser first, where
    # quarantine WOULD be set. It is guarded so a machine with no `xattr` is not
    # a failure.
    if command -v xattr >/dev/null 2>&1; then
        xattr -d com.apple.quarantine "${STAGED_BIN}" 2>/dev/null || true
    fi

    chmod +x "${STAGED_BIN}"

    step "Proving the binary runs"
    set +e
    reported="$("${STAGED_BIN}" --version 2>&1)"
    ver_status=$?
    set -e
    if [ "${ver_status}" -ne 0 ]; then
        die "The downloaded ${PRIMARY_BIN} binary did not run (exit ${ver_status}).
Output: ${reported}
The download verified against its published checksum, so the bytes are correct;
this binary does not execute on this machine.
What to do: report it against ${REPO}, naming macOS $(uname -r) ${TARGET}.
Nothing has been installed."
    fi
    ok "${reported}"
}

# ---------------------------------------------------------------------------
# Install, atomically.
#
# Why: CLAUDE.md documents that a plain `cp` over an on-PATH binary on macOS
# leaves a stale kernel cdhash cache, and the next exec is SIGKILL'd as an
# invalid signature — which looks exactly like an OOM kill. `mv` within the
# same filesystem is a rename(2): the destination inode is REPLACED rather than
# overwritten in place, so no stale cache is left behind and no reader ever
# observes a half-written file.
#
# Idempotent: re-running upgrades or replaces. A rename over an existing path
# is atomic, so a second run can no-op or replace but never half-install.
# ---------------------------------------------------------------------------
install_binaries() {
    INSTALL_DIR="${TRUSTY_AUDIT_INSTALL_DIR:-${DEFAULT_INSTALL_DIR}}"

    step "Installing to ${INSTALL_DIR}"
    if ! mkdir -p "${INSTALL_DIR}"; then
        die "Could not create the install directory: ${INSTALL_DIR}
What to do: choose a writable location with
  TRUSTY_AUDIT_INSTALL_DIR=\$HOME/bin
and re-run. Nothing has been installed."
    fi
    if [ ! -w "${INSTALL_DIR}" ]; then
        die "Install directory is not writable: ${INSTALL_DIR}
This installer never uses sudo and will not write outside a directory you own.
What to do: choose a writable location with
  TRUSTY_AUDIT_INSTALL_DIR=\$HOME/bin
and re-run. Nothing has been installed."
    fi

    # Stage inside the DESTINATION directory first, so the final `mv` is a
    # same-filesystem rename. A rename across filesystems degrades to a
    # copy-then-unlink, which is exactly the non-atomic behaviour being avoided.
    for name in "${PRIMARY_BIN}" "${ALIAS_BIN}"; do
        src="$(find "${EXTRACT_DIR}" -type f -name "${name}" | head -1)"
        if [ -z "${src}" ]; then
            # Only the primary is required; the alias is installed when present.
            if [ "${name}" = "${PRIMARY_BIN}" ]; then
                die "Binary '${name}' vanished from the staging directory before install.
Nothing has been installed."
            fi
            continue
        fi

        tmp_dest="${INSTALL_DIR}/.${name}.install.$$"
        cp "${src}" "${tmp_dest}"
        chmod +x "${tmp_dest}"
        if ! mv -f "${tmp_dest}" "${INSTALL_DIR}/${name}"; then
            rm -f "${tmp_dest}"
            die "Could not move ${name} into ${INSTALL_DIR}.
The previous contents of ${INSTALL_DIR}/${name} are unchanged.
What to do: check permissions on ${INSTALL_DIR} and re-run."
        fi
        ok "installed ${name}"
    done

    INSTALLED_BIN="${INSTALL_DIR}/${PRIMARY_BIN}"
}

# ---------------------------------------------------------------------------
# PATH check.
#
# Why: installing into a directory the operator's shell does not search is a
# silent failure — the binary is present and `trusty-audit` still says "command not
# found". Naming the exact line to add is the difference between actionable and
# not.
# ---------------------------------------------------------------------------
check_path() {
    step "Checking PATH"
    case ":${PATH}:" in
    *":${INSTALL_DIR}:"*)
        ok "${INSTALL_DIR} is on PATH"
        PATH_OK=1
        ;;
    *)
        PATH_OK=0
        say ""
        say "NOTE: ${INSTALL_DIR} is not on your PATH."
        say "      ${PRIMARY_BIN} is installed, but your shell will not find it by name."
        say ""
        say "      Add it for this session:"
        say "          export PATH=\"${INSTALL_DIR}:\$PATH\""
        say ""
        say "      Make it permanent (zsh is the macOS default shell):"
        say "          echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ~/.zshrc"
        say ""
        say "      Or run it by full path:"
        say "          ${INSTALLED_BIN}"
        say ""
        ;;
    esac
}

# ---------------------------------------------------------------------------
# Launch.
#
# Why: the requirement is "installs AND runs". Under `curl … | sh` the script's
# own stdin IS the script text, so a launched child that read stdin would get
# script bytes rather than the operator's typing. Redirecting the child's stdin
# from /dev/tty fixes that: /dev/tty is the controlling terminal regardless of
# what stdin was piped from. When there is no controlling terminal (CI, a
# non-interactive shell) there is nothing to attach, so the command is printed
# instead of launching something that would immediately fail on input.
#
# trusty-audit owns its own credential prompt (#5868) — this script does not collect,
# pass, or store a key.
# ---------------------------------------------------------------------------
launch() {
    if [ "${TRUSTY_AUDIT_NO_LAUNCH:-0}" = "1" ]; then
        say ""
        say "Installed. Not launching (TRUSTY_AUDIT_NO_LAUNCH=1). Start it with:"
        say "    ${PRIMARY_BIN}"
        return 0
    fi

    if [ "${PATH_OK}" -eq 1 ]; then
        launch_cmd="${PRIMARY_BIN}"
    else
        launch_cmd="${INSTALLED_BIN}"
    fi

    if [ -r /dev/tty ] && [ -w /dev/tty ]; then
        say ""
        step "Launching ${PRIMARY_BIN}"
        say ""
        # `exec` replaces this shell so trusty-audit owns the terminal directly and
        # its exit status becomes the installer's.
        exec "${INSTALLED_BIN}" </dev/tty
    fi

    say ""
    say "Installed, but not launched: there is no controlling terminal, so"
    say "${PRIMARY_BIN} could not prompt for the engagement credential."
    say ""
    say "Run it yourself:"
    say "    ${launch_cmd}"
    say ""
}

# ---------------------------------------------------------------------------
main() {
    say ""
    say "trusty-audit installer — ${REPO}"
    say ""

    require_host_tools
    check_platform

    STAGING_DIR="$(mktemp -d)"

    resolve_version
    download_and_verify
    extract_and_prove
    install_binaries
    check_path
    launch
}

main "$@"
