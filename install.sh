#!/bin/sh
# install.sh — trusty-tools bootstrap installer
#
# Usage:
#   curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh
#   curl -sSf <url> | sh -s -- -y        # non-interactive
#   TRUSTY_VERSION=0.2.0 sh install.sh   # pin version
#
# Why: Give users a rustup-style one-liner to install the `tctl` control-plane
#      binary and bootstrap the rest of the trusty-tools platform, without
#      requiring a Rust toolchain or manual download/verify/extract steps.
# What: Detects platform, resolves the latest (or pinned) trusty-controller
#      release, downloads + checksum-verifies the tarball, installs `tctl`,
#      optionally fixes PATH, then runs `tctl install` to finish bootstrap.
# Test: `sh -n install.sh` for POSIX syntax; `shellcheck --shell=sh install.sh`
#      for lint; manual run on macOS arm64 + Linux x86_64; run with
#      TRUSTY_VERSION pinned and TRUSTY_YES=1 to exercise non-interactive paths.
#
# Environment variables:
#   TRUSTY_VERSION         Pin tctl version (e.g. "0.2.0")
#   TRUSTY_INSTALL_DIR     Install dir (default: ~/.local/bin)
#   TRUSTY_YES             Set to 1 to skip all prompts (same as -y)
#   TRUSTY_NO_MODIFY_PATH  Set to 1 to skip PATH modification
#   TRUSTY_FORCE           Set to 1 to re-download even if already installed
#
# Security note: This script is served over HTTPS, which is the primary
# integrity guarantee. The installer script itself is not signed — if you
# require a higher assurance level, download and review the script manually
# before running it. All downloaded binaries are SHA-256 verified.

set -e

# ---------------------------------------------------------------------------
# Constants — keep all magic strings here, not scattered through the script.
# ---------------------------------------------------------------------------
REPO="bobmatnyc/trusty-tools"
CRATE="trusty-controller"
BIN="tctl"

REPO_URL="https://github.com/${REPO}"
API_RELEASES_URL="https://api.github.com/repos/${REPO}/releases"
RELEASE_DL_BASE="${REPO_URL}/releases/download"
TAG_PREFIX="${CRATE}-v"
# Default install dir. Alternative: ~/.cargo/bin if you have Rust installed.
DEFAULT_INSTALL_DIR="${HOME}/.local/bin"
FDA_DOCS_URL="${REPO_URL}/blob/main/CLAUDE.md"

# ---------------------------------------------------------------------------
# Output helpers — normal output to stdout, diagnostics to stderr.
# ---------------------------------------------------------------------------

# Why: Centralize error formatting so every abort path looks the same.
# What: Print a prefixed message to stderr.
# Test: Call err "x" and confirm it appears on fd 2 with the "error:" prefix.
err() {
    printf 'error: %s\n' "$*" >&2
}

# Why: Fatal failures should print and stop the script consistently.
# What: Emit an error message then exit non-zero.
# Test: Call die "x" and confirm exit code is 1 and message on stderr.
die() {
    err "$@"
    exit 1
}

# Why: Distinguish informational progress from warnings/errors.
# What: Print an info line to stdout.
# Test: Call say "hi" and confirm it appears on stdout.
say() {
    printf '%s\n' "$*"
}

# Why: Surface non-fatal advisories without aborting.
# What: Print a warning line to stderr.
# Test: Call warn "x" and confirm a "warning:" prefix on stderr.
warn() {
    printf 'warning: %s\n' "$*" >&2
}

# Why: Guard every external-tool dependency with a clear error.
# What: Return success if a command exists on PATH.
# Test: need_cmd curl on a system with curl returns 0; bogus name returns 1.
need_cmd() {
    command -v "$1" >/dev/null 2>&1
}

# ---------------------------------------------------------------------------
# Platform detection
# ---------------------------------------------------------------------------

# Why: The release ships only specific target triples; map the host to one or
#      fail with an actionable message rather than downloading a wrong asset.
# What: Echo the supported Rust target triple for this host, or die.
# Test: On macOS arm64 echoes aarch64-apple-darwin; on Linux x86_64 echoes
#      x86_64-unknown-linux-gnu; macOS Intel / Linux arm64 die with guidance.
detect_target() {
    _os="$(uname -s)"
    _arch="$(uname -m)"

    case "${_os}" in
        Darwin)
            case "${_arch}" in
                arm64 | aarch64)
                    printf 'aarch64-apple-darwin'
                    ;;
                x86_64)
                    # On Apple Silicon running under Rosetta 2, uname -m reports x86_64.
                    # Detect this case and guide the user to a native arm64 terminal.
                    if sysctl -n sysctl.proc_translated 2>/dev/null | grep -q '^1$'; then
                        die "Detected Rosetta 2 translation layer. Open a native arm64 terminal (e.g. launch Terminal from Finder without Rosetta) and re-run the installer."
                    fi
                    die "macOS Intel (x86_64) is not yet supported. Build from source: cargo install ${CRATE}"
                    ;;
                *)
                    die "unsupported macOS architecture '${_arch}'. Build from source: cargo install ${CRATE}"
                    ;;
            esac
            ;;
        Linux)
            case "${_arch}" in
                x86_64 | amd64)
                    printf 'x86_64-unknown-linux-gnu'
                    ;;
                aarch64 | arm64)
                    die "Linux aarch64 is not yet supported. Build from source: cargo install ${CRATE}"
                    ;;
                *)
                    die "unsupported Linux architecture '${_arch}'. Build from source: cargo install ${CRATE}"
                    ;;
            esac
            ;;
        *)
            die "unsupported operating system '${_os}'. Build from source: cargo install ${CRATE}"
            ;;
    esac
}

# Why: Checksum verification uses a different binary per OS; pick the right one.
# What: Echo "sha256sum" (Linux) or "shasum" (macOS), or die if neither exists.
# Test: On Linux echoes sha256sum; on macOS with shasum installed echoes shasum.
detect_sha_tool() {
    if need_cmd sha256sum; then
        printf 'sha256sum'
    elif need_cmd shasum; then
        printf 'shasum'
    else
        die "no SHA-256 tool found (need 'sha256sum' or 'shasum')"
    fi
}

# ---------------------------------------------------------------------------
# Version resolution
# ---------------------------------------------------------------------------

# Why: Allow pinning a version, otherwise discover the newest published
#      trusty-controller release so the one-liner always installs latest.
# What: Echo a bare version (e.g. "0.2.0"). Honors TRUSTY_VERSION if set.
# Test: With TRUSTY_VERSION=0.2.0 echoes 0.2.0 (no network); unset, parses the
#      releases API and echoes the first trusty-controller-v* tag's version.
resolve_version() {
    if [ -n "${TRUSTY_VERSION:-}" ]; then
        # Tolerate a leading tag prefix or 'v' if the user pasted a full tag.
        _v="${TRUSTY_VERSION#"${TAG_PREFIX}"}"
        _v="${_v#v}"
        # Validate: reject pre-release suffixes like '0.3.0-rc1' which would 404.
        printf '%s' "${_v}" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$' \
            || die "TRUSTY_VERSION '${TRUSTY_VERSION}' is not a valid X.Y.Z version (got: '${_v}')"
        printf '%s' "${_v}"
        return 0
    fi

    say "Resolving latest ${CRATE} release..." >&2

    # Fetch the releases list and extract the first trusty-controller-v* tag.
    # No jq required — grep/sed only.
    _ver="$(
        curl -sSfL "${API_RELEASES_URL}" \
            | grep '"tag_name"' \
            | grep "${TAG_PREFIX}" \
            | head -1 \
            | sed "s/.*\"${TAG_PREFIX}\\([^\"]*\\)\".*/\\1/"
    )" || die "failed to query GitHub releases API at ${API_RELEASES_URL}"

    if [ -z "${_ver}" ]; then
        die "no ${TAG_PREFIX}* release found at ${API_RELEASES_URL}"
    fi

    # Sanity-check it looks like a strict X.Y.Z version (digits and dots only).
    # Use anchored grep -E rather than a case glob to reject suffixes like '-rc1'.
    printf '%s' "${_ver}" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$' \
        || die "unexpected version string from API; got: '${_ver}' (expected X.Y.Z)"

    printf '%s' "${_ver}"
}

# ---------------------------------------------------------------------------
# Prompt handling
# ---------------------------------------------------------------------------

# Why: Honor non-interactive mode (CI, piped curl) while still letting an
#      interactive user confirm side-effecting steps.
# What: Return 0 (yes) without asking when ASSUME_YES=1; otherwise prompt.
#      A non-tty stdin with no -y/TRUSTY_YES defaults to "no" to stay safe.
# Test: With ASSUME_YES=1 returns 0 immediately; interactively, "y" returns 0
#      and anything else returns 1.
confirm() {
    _prompt="$1"
    if [ "${ASSUME_YES}" = "1" ]; then
        return 0
    fi
    # If stdin is not a terminal (e.g. piped curl without -y), do not block.
    if [ ! -t 0 ]; then
        warn "non-interactive shell and no -y/TRUSTY_YES; skipping: ${_prompt}"
        return 1
    fi
    printf '%s [y/N] ' "${_prompt}"
    read -r _reply
    case "${_reply}" in
        y | Y | yes | YES)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

# ---------------------------------------------------------------------------
# PATH handling
# ---------------------------------------------------------------------------

# Why: Installing into ~/.local/bin is useless if it is not on PATH; help the
#      user wire it up, but never modify their shell config without consent.
# What: If install dir is not on PATH, append an export line to the detected
#      shell RC file (after confirmation). Skipped when TRUSTY_NO_MODIFY_PATH=1.
# Test: With a dir already on PATH, returns immediately. With a dir not on PATH
#      and ASSUME_YES=1, appends an export line to the detected RC file.
maybe_update_path() {
    _dir="$1"

    # Validate: only allow safe path characters to prevent shell RC injection.
    case "${_dir}" in
        *[!A-Za-z0-9/_.~-]*)
            warn "TRUSTY_INSTALL_DIR contains unsafe characters; skipping PATH modification"
            return 0
            ;;
    esac

    case ":${PATH}:" in
        *":${_dir}:"*)
            # Already on PATH — nothing to do.
            return 0
            ;;
    esac

    if [ "${TRUSTY_NO_MODIFY_PATH:-}" = "1" ]; then
        return 0
    fi

    # Detect the RC file from the login shell; default to ~/.profile.
    _shell_name="$(basename "${SHELL:-/bin/sh}")"
    case "${_shell_name}" in
        zsh)
            _rc="${HOME}/.zshrc"
            ;;
        bash)
            _rc="${HOME}/.bashrc"
            ;;
        *)
            _rc="${HOME}/.profile"
            ;;
    esac

    say ""
    if confirm "Add ${_dir} to your PATH in ${_rc}?"; then
        {
            printf '\n# Added by trusty-tools install.sh\n'
            # shellcheck disable=SC2016
            # Single-quote _dir so it is written verbatim; $PATH expands at
            # shell startup, not now. The allowlist guard above already rejects
            # single-quote characters from _dir.
            printf "export PATH='%s':\$PATH\n" "${_dir}"
        } >>"${_rc}"
        say "Added ${_dir} to PATH in ${_rc}. Restart your shell or run:"
        say "    export PATH=\"${_dir}:\$PATH\""
    else
        say "Skipped PATH modification. Add this to your shell config manually:"
        say "    export PATH=\"${_dir}:\$PATH\""
    fi
}

# ---------------------------------------------------------------------------
# Download + verify + install
# ---------------------------------------------------------------------------

# Why: Verify integrity before trusting a downloaded binary; never install an
#      asset whose checksum does not match the published .sha256.
# What: Download .sha256 + tarball into the temp dir, verify with the detected
#      sha tool, then extract the `tctl` binary into the install dir with exec
#      permissions.
# Test: Point at a known release; on match the binary lands in the install dir;
#      corrupt the tarball and confirm verification aborts non-zero.
download_and_install() {
    _version="$1"
    _target="$2"
    _install_dir="$3"
    _sha_tool="$4"

    _archive="${CRATE}-${_version}-${_target}.tar.gz"
    _sha_file="${_archive}.sha256"
    _extract_dir="${CRATE}-${_version}-${_target}"
    _url="${RELEASE_DL_BASE}/${TAG_PREFIX}${_version}/${_archive}"
    _sha_url="${_url}.sha256"

    # Always HTTPS; -sSfL fails loudly on HTTP errors and follows redirects.
    say "Downloading checksum ${_sha_file}..."
    curl -sSfL "${_sha_url}" -o "${TMP_DIR}/${_sha_file}" \
        || die "failed to download checksum from ${_sha_url}"

    say "Downloading ${_archive}..."
    curl -sSfL "${_url}" -o "${TMP_DIR}/${_archive}" \
        || die "failed to download archive from ${_url}"

    say "Verifying checksum..."
    # The published .sha256 is standard shasum format: "{hash}  {filename}".
    # Run the checker from inside the temp dir so the embedded bare filename
    # resolves correctly, using -c for an authoritative pass/fail.
    (
        cd "${TMP_DIR}" || exit 1
        if [ "${_sha_tool}" = "sha256sum" ]; then
            sha256sum -c "${_sha_file}"
        else
            shasum -a 256 -c "${_sha_file}"
        fi
    ) >/dev/null || die "checksum verification failed for ${_archive}; aborting install"
    say "Checksum OK."

    say "Extracting..."
    tar -xzf "${TMP_DIR}/${_archive}" -C "${TMP_DIR}" \
        || die "failed to extract ${_archive}"

    _src_bin="${TMP_DIR}/${_extract_dir}/${BIN}"
    if [ ! -f "${_src_bin}" ]; then
        die "expected binary not found in archive: ${_extract_dir}/${BIN}"
    fi

    mkdir -p "${_install_dir}" || die "failed to create install dir ${_install_dir}"
    _tmp_bin="${_install_dir}/.${BIN}.tmp.$$"
    if ! install -m 0755 "${_src_bin}" "${_tmp_bin}" 2>/dev/null; then
        cp "${_src_bin}" "${_tmp_bin}" \
            || die "failed to copy binary to ${_install_dir}"
        chmod 0755 "${_tmp_bin}" \
            || die "failed to set permissions on temp install binary"
    fi
    mv "${_tmp_bin}" "${_install_dir}/${BIN}" || {
        rm -f "${_tmp_bin}"
        die "failed to move binary into place at ${_install_dir}/${BIN}"
    }

    say "Installed ${BIN} to ${_install_dir}/${BIN}"
}

# ---------------------------------------------------------------------------
# Post-install bootstrap + notices
# ---------------------------------------------------------------------------

# Why: tctl bootstraps the rest of the platform; running it completes setup.
#      Use the freshly-installed binary by absolute path so we do not depend
#      on the (possibly not-yet-updated) PATH.
# What: Invoke `tctl install` (or `tctl install -y` in non-interactive mode),
#      then print the macOS Full Disk Access note (macOS only).
# Test: Confirm the binary at its absolute path is invoked; in non-interactive
#      mode the -y flag is passed; on macOS the FDA box is printed.
post_install() {
    _bin_path="$1"

    say ""
    say "Bootstrapping the platform with '${BIN} install'..."
    if [ "${ASSUME_YES}" = "1" ]; then
        "${_bin_path}" install -y || warn "'${BIN} install -y' returned non-zero; review output above"
    else
        "${_bin_path}" install || warn "'${BIN} install' returned non-zero; review output above"
    fi

    # macOS Full Disk Access note — daemons need FDA to read external volumes.
    if [ "$(uname -s)" = "Darwin" ]; then
        say ""
        say "==================================================================="
        say "macOS note: Daemons need Full Disk Access to read external volumes."
        say "After installation, go to:"
        say "  System Settings -> Privacy & Security -> Full Disk Access"
        say "Remove and re-add each daemon binary if it loses access."
        say "See: ${FDA_DOCS_URL}"
        say "==================================================================="
    fi
}

# Why: Leave the user with concrete next steps regardless of platform/path.
# What: Print the final next-steps box.
# Test: Run end-to-end and confirm the box is the last thing printed.
print_next_steps() {
    say ""
    say "==================================================================="
    say "Installation complete!"
    say ""
    say "Next steps:"
    say "  ${BIN} status          # check all daemon statuses"
    say "  ${BIN} stack health    # full platform health check"
    say ""
    say "Restart Claude Code to load the new MCP servers."
    say ""
    say "Docs: ${REPO_URL}"
    say "==================================================================="
}

# ---------------------------------------------------------------------------
# Cleanup
# ---------------------------------------------------------------------------

# Why: Never leave temp downloads behind, even on failure.
# What: Remove the temp working dir if it was created.
# Test: Trigger any exit and confirm TMP_DIR no longer exists.
cleanup() {
    if [ -n "${TMP_DIR:-}" ] && [ -d "${TMP_DIR}" ]; then
        rm -rf "${TMP_DIR}"
    fi
}

# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------

# Why: Wrap logic in a function so locals work and the script can be piped
#      safely (curl | sh) — the body runs only after the whole file downloads.
# What: Parse flags, detect platform, resolve version, download/verify/install
#      (skipping when already current), fix PATH, then bootstrap and summarize.
# Test: Run end-to-end on a supported platform; run with -y and TRUSTY_VERSION;
#      re-run to exercise the idempotent skip path.
main() {
    trap 'cleanup' EXIT

    # Default assume-yes from env; the -y flag below can also set it.
    ASSUME_YES="${TRUSTY_YES:-0}"
    FORCE_INSTALL="${TRUSTY_FORCE:-0}"

    # Parse flags (supports both `sh install.sh -y` and `| sh -s -- -y`).
    while [ "$#" -gt 0 ]; do
        case "$1" in
            -y | --yes)
                ASSUME_YES="1"
                ;;
            --force)
                FORCE_INSTALL="1"
                ;;
            -h | --help)
                say "Usage: install.sh [-y]"
                say ""
                say "Environment:"
                say "  TRUSTY_VERSION           Pin a specific ${BIN} version"
                say "  TRUSTY_INSTALL_DIR       Install dir (default: ${DEFAULT_INSTALL_DIR})"
                say "  TRUSTY_YES=1             Skip all prompts (same as -y)"
                say "  TRUSTY_NO_MODIFY_PATH=1  Do not modify shell PATH"
                exit 0
                ;;
            *)
                die "unknown argument '$1' (use -y for non-interactive)"
                ;;
        esac
        shift
    done

    # Required tools.
    need_cmd curl || die "'curl' is required but not found on PATH"
    need_cmd tar || die "'tar' is required but not found on PATH"

    _target="$(detect_target)"
    _sha_tool="$(detect_sha_tool)"
    _install_dir="${TRUSTY_INSTALL_DIR:-${DEFAULT_INSTALL_DIR}}"

    _version="$(resolve_version)"
    say "Target:      ${_target}"
    say "Version:     ${_version}"
    say "Install dir: ${_install_dir}"

    # Idempotency: if tctl is on PATH and already the target version, skip
    # download. We still run `tctl install` below (it is itself idempotent).
    _existing="$(command -v "${BIN}" 2>/dev/null || true)"
    _need_install="1"
    if [ "${FORCE_INSTALL}" != "1" ] && [ -n "${_existing}" ]; then
        _existing_ver="$("${_existing}" --version 2>/dev/null | awk 'NR==1{print $2}' || true)"
        if [ "${_existing_ver}" = "${_version}" ]; then
            # Security note: this skips checksum re-verification of the existing binary.
            # Run with --force / TRUSTY_FORCE=1 to always re-download and re-verify.
            say "${BIN} ${_version} is already installed, skipping download."
            warn "Skipping integrity check for existing ${BIN} ${_version}. Use --force to re-verify."
            _need_install="0"
        fi
    fi

    if [ "${_need_install}" = "1" ]; then
        # Create temp working dir now that we know we will download.
        TMP_DIR="$(mktemp -d 2>/dev/null || mktemp -d -t trusty-install)" \
            || die "failed to create temp directory"

        download_and_install "${_version}" "${_target}" "${_install_dir}" "${_sha_tool}"
        maybe_update_path "${_install_dir}"
        _bin_path="${_install_dir}/${BIN}"
    else
        _bin_path="${_existing}"
    fi

    post_install "${_bin_path}"
    print_next_steps
}

main "$@"
