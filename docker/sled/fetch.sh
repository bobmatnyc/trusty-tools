#!/usr/bin/env bash
# fetch.sh TARGET_DIR — download the three released linux x86_64 tarballs and
# verify every sha256. Fails closed: any missing file or mismatch exits non-zero
# and deletes the tarballs. Run on a host with gh (or curl); never in the image.
# Copied from docker/cloudtest. Refs #11054 #11073
set -euo pipefail

REPO="bobmatnyc/trusty-tools"
# crate:version pairs — bump here to roll the image.
PKGS=("trusty-mpm:1.12.2" "trusty-memory:0.36.7" "trusty-search:0.63.1")
TARGET="x86_64-unknown-linux-gnu"

dest="${1:-}"
[ -n "${dest}" ] || { echo "usage: fetch.sh TARGET_DIR" >&2; exit 2; }
mkdir -p "${dest}"
dest="$(cd "${dest}" && pwd)"

GH="$(command -v gh || true)"
[ -z "${GH}" ] && [ -x "${HOME}/.local/bin/gh" ] && GH="${HOME}/.local/bin/gh"

fail() {
    echo "fetch.sh: FAIL: $*" >&2
    rm -f "${dest}"/*.tar.gz "${dest}"/*.tar.gz.sha256
    exit 1
}

for p in "${PKGS[@]}"; do
    crate="${p%%:*}"; ver="${p##*:}"
    tag="${crate}-v${ver}"
    base="${crate}-${ver}-${TARGET}.tar.gz"
    rm -f "${dest}/${base}" "${dest}/${base}.sha256"
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
done

# Verify every tarball. The .sha256 may name the file with a path; compare the
# hash field against a freshly computed one so the check does not depend on cwd.
if command -v sha256sum >/dev/null 2>&1; then
    hasher=(sha256sum)
else
    hasher=(shasum -a 256)
fi
for p in "${PKGS[@]}"; do
    crate="${p%%:*}"; ver="${p##*:}"
    base="${crate}-${ver}-${TARGET}.tar.gz"
    want="$(awk 'NR==1{print $1}' "${dest}/${base}.sha256")"
    got="$("${hasher[@]}" "${dest}/${base}" | awk '{print $1}')"
    case "${want}" in
        [0-9a-f]*) [ "${#want}" -eq 64 ] || fail "malformed checksum for ${base}" ;;
        *) fail "malformed checksum for ${base}" ;;
    esac
    [ "${want}" = "${got}" ] || fail "sha256 mismatch for ${base}: want ${want} got ${got}"
    echo "OK  ${base}  ${got}"
done
echo "fetch.sh: all tarballs verified in ${dest}"
