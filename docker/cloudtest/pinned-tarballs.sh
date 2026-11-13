#!/usr/bin/env bash
# pinned-tarballs.sh — read tarballs.pin and unpack only the pinned tarballs.
#
# Why: #11156 B4. The old unpack stage ran `sha256sum -c ./*.sha256`, which
#   checks only tarballs that carry a .sha256, then extracted every *.tar.gz and
#   took the first binary `find` returned. A stale or planted tarball in dl/
#   could supply tm, trusty-memory or trusty-search while the check passed.
# What:
#   pinned-tarballs.sh pins PIN_FILE
#       Validate PIN_FILE and print one "<tag> <file>" line per pinned tarball.
#       fetch.sh reads its download list from here.
#   pinned-tarballs.sh unpack PIN_FILE DL_DIR OUT_DIR BINARY...
#       Fail on any *.tar.gz or *.tar.gz.sha256 in DL_DIR that is not pinned,
#       and on a pinned tarball or .sha256 that is missing. Compare each pinned
#       tarball's sha256 with the first field of its .sha256, extract each into
#       its own directory, and install each BINARY into OUT_DIR. A BINARY name
#       found zero times or more than once, or not as a regular file, fails.
# Test: scripts/check_sled_pinned_tarballs.sh. The same file sits in
#   docker/sled and docker/cloudtest (one per build context); that check
#   requires the two copies to be identical.
set -euo pipefail

die() { echo "pinned-tarballs.sh: FAIL: $*" >&2; exit 1; }
usage() {
    echo "usage: pinned-tarballs.sh pins PIN_FILE" >&2
    echo "       pinned-tarballs.sh unpack PIN_FILE DL_DIR OUT_DIR BINARY..." >&2
    exit 2
}

# read_pins PIN_FILE — fill PIN_TAGS and PIN_FILES from the validated pin file.
read_pins() {
    local pin="$1" tag file extra
    [ -f "${pin}" ] || die "pin file not found: ${pin}"
    PIN_TAGS=(); PIN_FILES=()
    while read -r tag file extra || [ -n "${tag}" ]; do
        case "${tag}" in ''|'#'*) continue ;; esac
        [ -n "${file}" ] && [ -z "${extra}" ] || die "malformed pin line: ${tag} ${file} ${extra}"
        case "${tag}${file}" in *[!A-Za-z0-9._-]*) die "bad character in pin line: ${tag} ${file}" ;; esac
        case "${file}" in .*|*..*) die "bad pinned file name: ${file}" ;; esac
        case "${file}" in *.tar.gz) ;; *) die "pinned file is not a .tar.gz: ${file}" ;; esac
        is_pinned "${file}" && die "file pinned twice: ${file}"
        PIN_TAGS+=("${tag}"); PIN_FILES+=("${file}")
    done < "${pin}"
    [ "${#PIN_FILES[@]}" -gt 0 ] || die "no tarballs pinned in ${pin}"
}

is_pinned() {
    local f
    for f in ${PIN_FILES[@]+"${PIN_FILES[@]}"}; do
        [ "${f}" = "$1" ] && return 0
    done
    return 1
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

cmd_unpack() {
    local pin="$1" dl="$2" out="$3"; shift 3
    [ "$#" -gt 0 ] || usage
    read_pins "${pin}"
    [ -d "${dl}" ] || die "no such directory: ${dl}"

    # Any tarball or checksum the pin file does not name fails the build.
    local f name
    for f in "${dl}"/*.tar.gz "${dl}"/*.tar.gz.sha256; do
        [ -e "${f}" ] || [ -L "${f}" ] || continue
        name="${f##*/}"
        is_pinned "${name%.sha256}" || die "unpinned file in ${dl}: ${name}"
    done

    local want got i=0
    for f in "${PIN_FILES[@]}"; do
        [ -f "${dl}/${f}" ] && [ ! -L "${dl}/${f}" ] || die "missing pinned tarball: ${f}"
        [ -f "${dl}/${f}.sha256" ] || die "missing ${f}.sha256"
        want="$(awk 'NR==1{print $1}' "${dl}/${f}.sha256")"
        case "${want}" in *[!0-9a-f]*|'') die "malformed checksum for ${f}" ;; esac
        [ "${#want}" -eq 64 ] || die "malformed checksum for ${f}"
        got="$(sha256_of "${dl}/${f}")"
        [ "${want}" = "${got}" ] || die "sha256 mismatch for ${f}: want ${want} got ${got}"
        echo "OK  ${f}  ${got}"
    done

    # Global, so the EXIT trap still sees it after this function returns.
    WORK="$(mktemp -d)"
    trap 'rm -rf "${WORK}"' EXIT
    for f in "${PIN_FILES[@]}"; do
        i=$((i + 1))
        mkdir -p "${WORK}/${i}"
        tar -xzf "${dl}/${f}" -C "${WORK}/${i}"
    done

    mkdir -p "${out}"
    local b m one n
    for b in "$@"; do
        n=0; one=""
        while IFS= read -r m; do
            [ -n "${m}" ] || continue
            n=$((n + 1)); one="${m}"
        done <<< "$(find "${WORK}" -name "${b}")"
        [ "${n}" -eq 1 ] || die "binary ${b} found ${n} times in the pinned tarballs (want exactly 1)"
        [ -f "${one}" ] && [ ! -L "${one}" ] || die "binary ${b} is not a regular file"
        install -m 0755 "${one}" "${out}/${b}"
    done
}

case "${1:-}" in
    pins)
        [ "$#" -eq 2 ] || usage
        read_pins "$2"
        for i in "${!PIN_FILES[@]}"; do echo "${PIN_TAGS[$i]} ${PIN_FILES[$i]}"; done
        ;;
    unpack)
        [ "$#" -ge 5 ] || usage
        shift
        cmd_unpack "$@"
        ;;
    *) usage ;;
esac
