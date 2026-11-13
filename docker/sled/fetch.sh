#!/usr/bin/env bash
# fetch.sh [DL_DIR] — download the release tarballs tarballs.pin names into
# this directory's dl/ and verify every sha256. Fails closed: any missing file
# or mismatch exits non-zero and deletes the tarballs. Run on a host with gh
# (or curl); never in the image.
# Copied from docker/cloudtest. Refs #11054 #11073
# #11156 B4: the list comes from tarballs.pin, the same file the build reads.
# Stale *.tar.gz / *.tar.gz.sha256 files are cleared first, and only in this
# script's own dl/; any other DL_DIR is refused.
set -euo pipefail

REPO="bobmatnyc/trusty-tools"
here="$(cd "$(dirname "$0")" && pwd -P)"
dest="${here}/dl"
[ ! -L "${dest}" ] || { echo "fetch.sh: ${dest} is a symlink" >&2; exit 2; }
if [ "$#" -gt 1 ]; then echo "usage: fetch.sh [DL_DIR]" >&2; exit 2; fi
if [ -n "${1:-}" ]; then
    mkdir -p "${dest}"
    arg="$(cd "$1" 2>/dev/null && pwd -P || true)"
    [ "${arg}" = "${dest}" ] || { echo "fetch.sh: DL_DIR must be ${dest}" >&2; exit 2; }
fi
mkdir -p "${dest}"

# "<tag> <file>" lines, validated by the same script the build runs.
pins="$("${here}/pinned-tarballs.sh" pins "${here}/tarballs.pin")"

GH="$(command -v gh || true)"
[ -z "${GH}" ] && [ -x "${HOME}/.local/bin/gh" ] && GH="${HOME}/.local/bin/gh"

clear_dl() { rm -f "${dest}"/*.tar.gz "${dest}"/*.tar.gz.sha256; }
fail() {
    echo "fetch.sh: FAIL: $*" >&2
    clear_dl
    exit 1
}

clear_dl
while read -r tag base; do
    if [ -n "${GH}" ]; then
        "${GH}" release download "${tag}" --repo "${REPO}" --dir "${dest}" \
            --pattern "${base}" --pattern "${base}.sha256" --clobber \
            || fail "gh release download ${tag}"
    elif command -v curl >/dev/null 2>&1; then
        url="https://github.com/${REPO}/releases/download/${tag}"
        curl -fsSL -o "${dest}/${base}" "${url}/${base}" || fail "curl ${base}"
        curl -fsSL -o "${dest}/${base}.sha256" "${url}/${base}.sha256" || fail "curl ${base}.sha256"
    else
        fail "neither gh nor curl available"
    fi
    [ -s "${dest}/${base}" ] && [ -s "${dest}/${base}.sha256" ] || fail "missing ${base} or its .sha256"
done <<< "${pins}"

# Verify every tarball. The .sha256 may name the file with a path; compare the
# hash field against a freshly computed one so the check does not depend on cwd.
if command -v sha256sum >/dev/null 2>&1; then
    hasher=(sha256sum)
else
    hasher=(shasum -a 256)
fi
while read -r _tag base; do
    want="$(awk 'NR==1{print $1}' "${dest}/${base}.sha256")"
    got="$("${hasher[@]}" "${dest}/${base}" | awk '{print $1}')"
    case "${want}" in
        [0-9a-f]*) [ "${#want}" -eq 64 ] || fail "malformed checksum for ${base}" ;;
        *) fail "malformed checksum for ${base}" ;;
    esac
    [ "${want}" = "${got}" ] || fail "sha256 mismatch for ${base}: want ${want} got ${got}"
    echo "OK  ${base}  ${got}"
done <<< "${pins}"
echo "fetch.sh: all tarballs verified in ${dest}"
