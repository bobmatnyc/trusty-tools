#!/usr/bin/env bash
# check_sled_pinned_tarballs.sh — the sled and cloudtest images extract only the
# pinned, hash-verified release tarballs (#11156 B4).
#
# Why: the old unpack stage hash-checked only tarballs that had a .sha256,
#   extracted every *.tar.gz in dl/ and took the first binary `find` returned,
#   and fetch.sh left old tarballs in dl/. A stale or planted tarball could
#   supply tm, trusty-memory or trusty-search while the hash check passed.
# What: for docker/sled and docker/cloudtest, without docker:
#   1. static: the Dockerfile unpack stage runs pinned-tarballs.sh against
#      tarballs.pin and carries none of the old glob-extract shapes; fetch.sh
#      reads tarballs.pin; the two pinned-tarballs.sh copies are identical.
#   2. unpack: lifts the RUN command out of the Dockerfile unpack stage,
#      maps /dl /out /x /pin to a scratch directory and runs it against
#      synthetic tarballs: a clean dl/ passes; a stale tarball, a planted
#      tarball without .sha256, a missing .sha256, a tampered tarball and a
#      binary present in two tarballs each fail.
#   3. fetch: runs fetch.sh with a stub gh. It must leave only the pinned files
#      in its own dl/ and refuse any other directory, deleting nothing there.
# Test: run this script; it exits 1 on the pre-#11156 tree.
# Usage: scripts/check_sled_pinned_tarballs.sh [REPO_ROOT]
set -euo pipefail

root="$(cd "${1:-$(dirname "$0")/..}" && pwd)"
S="$(mktemp -d)"
trap 'rm -rf "${S}"' EXIT
export S

pass=0; failed=0
ok()  { echo "PASS  $*"; pass=$((pass + 1)); }
bad() { echo "FAIL  $*"; failed=$((failed + 1)); }

sha() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
    else shasum -a 256 "$1" | awk '{print $1}'; fi
}

# Pinned tarball names for a context. Trees older than #11156 have no
# tarballs.pin; fall back to the names fetch.sh hard-coded there.
pinned_files() {
    if [ -f "$1/tarballs.pin" ]; then
        awk '$1 !~ /^#/ && NF == 2 {print $2}' "$1/tarballs.pin"
    else
        printf '%s\n' trusty-mpm-1.12.2-x86_64-unknown-linux-gnu.tar.gz \
            trusty-memory-0.36.7-x86_64-unknown-linux-gnu.tar.gz \
            trusty-search-0.63.1-x86_64-unknown-linux-gnu.tar.gz
    fi
}

bins_for() {
    case "$1" in
        trusty-mpm-*) echo "tm trusty-mpm" ;;
        trusty-memory-*) echo "trusty-memory" ;;
        trusty-search-*) echo "trusty-search trusty-embedderd trusty-embedderd-py" ;;
        *) echo "tm" ;;
    esac
}

# mk_tarball DIR FILE BIN... — FILE in DIR holding <stem>/<bin> for each BIN,
# each binary's content naming FILE, plus FILE.sha256.
mk_tarball() {
    local dir="$1" file="$2"; shift 2
    local stem="${file%.tar.gz}" st b
    st="$(mktemp -d "${S}/st.XXXXXX")"
    mkdir -p "${st}/${stem}"
    for b in "$@"; do printf '%s:%s\n' "${file}" "${b}" > "${st}/${stem}/${b}"; done
    tar -czf "${dir}/${file}" -C "${st}" "${stem}"
    printf '%s  %s\n' "$(sha "${dir}/${file}")" "${file}" > "${dir}/${file}.sha256"
}

# clean_dl CTX DIR — DIR holds every pinned tarball of CTX with its .sha256.
clean_dl() {
    local f
    mkdir -p "$2"
    while read -r f; do
        # shellcheck disable=SC2046
        mk_tarball "$2" "${f}" $(bins_for "${f}")
    done < <(pinned_files "$1")
}

# The RUN command of the Dockerfile's unpack stage, continuation lines joined.
unpack_run() {
    awk '/^FROM .* AS unpack/ {s = 1; next}
         s && /^FROM / {exit}
         s && /^RUN / {r = 1}
         r {l = $0; c = sub(/\\$/, "", l); sub(/^RUN /, "", l); printf "%s ", l; if (!c) exit}' "$1/Dockerfile"
}

# run_unpack CTX DL WORK — run the unpack stage's RUN with its absolute paths
# mapped under WORK and DL as /dl. Output goes to WORK/log.
run_unpack() {
    local ctx="$1" dl="$2" work="$3" cmd
    mkdir -p "${work}/pin"
    rm -rf "${work}/dl"; cp -R "${dl}" "${work}/dl"
    for f in tarballs.pin pinned-tarballs.sh; do
        [ -f "${ctx}/${f}" ] && cp "${ctx}/${f}" "${work}/pin/${f}"
    done
    cmd="$(unpack_run "${ctx}" | W="${work}" perl -pe 's{(?<![\w./-])/(dl|out|x|pin)(?=[/\s;"]|$)}{$ENV{W}/$1}g')"
    (cd "${work}" && sh -c "${cmd}") > "${work}/log" 2>&1
}

expect_unpack() { # CTX NAME WANT(pass|fail) DL [REASON — text the failure log must carry]
    local ctx="$1" name="$2" want="$3" dl="$4" reason="${5:-}" work rc=0
    work="$(mktemp -d "${S}/w.XXXXXX")"
    run_unpack "${ctx}" "${dl}" "${work}" || rc=$?
    if { [ "${want}" = pass ] && [ "${rc}" -eq 0 ]; } ||
       { [ "${want}" = fail ] && [ "${rc}" -ne 0 ] && grep -qF -- "${reason}" "${work}/log"; }; then
        ok "${ctx#"${root}/"} unpack: ${name} -> ${want}"
    else
        bad "${ctx#"${root}/"} unpack: ${name}: want ${want}, exit ${rc}; last log lines:"
        tail -n 3 "${work}/log" | sed 's/^/        /'
    fi
    LAST_WORK="${work}"
}

check_static() {
    local ctx="$1" rel="${1#"${root}/"}" stage
    stage="$(awk '/^FROM .* AS unpack/ {s = 1} s && /^FROM / && !/AS unpack/ {exit} s' "${ctx}/Dockerfile")"
    if grep -qE 'sha256sum -c|head -n 1|for t in /dl/\*\.tar\.gz' <<< "${stage}"; then
        bad "${rel}/Dockerfile unpack stage still globs, sha256sum -c's or head -n 1's the tarballs"
    else ok "${rel}/Dockerfile unpack stage has no glob extract"; fi
    if grep -qF 'COPY tarballs.pin pinned-tarballs.sh /pin/' <<< "${stage}" &&
       grep -qF 'pinned-tarballs.sh unpack /pin/tarballs.pin /dl /out' <<< "${stage}"; then
        ok "${rel}/Dockerfile unpacks through pinned-tarballs.sh and tarballs.pin"
    else bad "${rel}/Dockerfile does not unpack through pinned-tarballs.sh and tarballs.pin"; fi
    if grep -qF 'pinned-tarballs.sh" pins' "${ctx}/fetch.sh" && ! grep -q '^PKGS=' "${ctx}/fetch.sh"; then
        ok "${rel}/fetch.sh reads tarballs.pin"
    else bad "${rel}/fetch.sh does not read its list from tarballs.pin"; fi
}

check_unpack() {
    local ctx="$1" base="${S}/fx.$2" d first second
    clean_dl "${ctx}" "${base}/clean"
    first="$(pinned_files "${ctx}" | sed -n 1p)"
    second="$(pinned_files "${ctx}" | sed -n 2p)"

    expect_unpack "${ctx}" "clean dl/" pass "${base}/clean"
    if [ -f "${LAST_WORK}/out/tm" ] && grep -qF "${first}:tm" "${LAST_WORK}/out/tm"; then
        ok "${ctx#"${root}/"} unpack: clean dl/ installs tm from ${first}"
    else bad "${ctx#"${root}/"} unpack: clean dl/ did not install tm from ${first}"; fi

    d="${base}/stale"; cp -R "${base}/clean" "${d}"
    mk_tarball "${d}" trusty-mpm-1.0.0-x86_64-unknown-linux-gnu.tar.gz tm trusty-mpm
    expect_unpack "${ctx}" "stale tarball with its own .sha256" fail "${d}" \
        "unpinned file in"

    d="${base}/planted"; cp -R "${base}/clean" "${d}"
    mk_tarball "${d}" aaa-planted.tar.gz tm trusty-memory trusty-search
    rm -f "${d}/aaa-planted.tar.gz.sha256"
    expect_unpack "${ctx}" "planted tarball without .sha256" fail "${d}" \
        "unpinned file in"

    d="${base}/nosha"; cp -R "${base}/clean" "${d}"
    rm -f "${d}/${first}.sha256"
    expect_unpack "${ctx}" "pinned tarball missing its .sha256" fail "${d}" \
        "missing ${first}.sha256"

    d="${base}/tamper"; cp -R "${base}/clean" "${d}"
    mk_tarball "${S}" "${first}" tm trusty-mpm evil
    cp "${S}/${first}" "${d}/${first}"
    expect_unpack "${ctx}" "tarball differs from its .sha256" fail "${d}" \
        "sha256 mismatch for ${first}"

    d="${base}/dup"; mkdir -p "${d}"; cp -R "${base}/clean/." "${d}/"
    # shellcheck disable=SC2046
    mk_tarball "${d}" "${second}" $(bins_for "${second}") tm
    expect_unpack "${ctx}" "binary tm in two pinned tarballs" fail "${d}" \
        "binary tm found 2 times"
}

check_fetch() {
    local ctx="$1" rel="${1#"${root}/"}" t="${S}/fetch.$2" f rc=0 have want
    mkdir -p "${t}/ctx/dl" "${t}/fixture" "${t}/bin" "${t}/outside"
    for f in fetch.sh tarballs.pin pinned-tarballs.sh; do
        [ -f "${ctx}/${f}" ] && cp -p "${ctx}/${f}" "${t}/ctx/${f}"
    done
    clean_dl "${ctx}" "${t}/fixture"
    mk_tarball "${t}/ctx/dl" trusty-mpm-1.0.0-x86_64-unknown-linux-gnu.tar.gz tm trusty-mpm
    mk_tarball "${t}/outside" keep-me.tar.gz tm
    cat > "${t}/bin/gh" <<'STUB'
#!/usr/bin/env bash
# Stub `gh release download`: copy each --pattern file from $FIXTURE to --dir.
dir=""; pats=()
while [ "$#" -gt 0 ]; do
    case "$1" in
        --dir) dir="$2"; shift ;;
        --pattern) pats+=("$2"); shift ;;
    esac
    shift
done
for p in "${pats[@]}"; do cp "${FIXTURE}/${p}" "${dir}/${p}" || exit 1; done
STUB
    chmod +x "${t}/bin/gh"

    FIXTURE="${t}/fixture" PATH="${t}/bin:${PATH}" "${t}/ctx/fetch.sh" "${t}/ctx/dl" \
        > "${t}/log" 2>&1 || rc=$?
    have="$(find "${t}/ctx/dl" -maxdepth 1 \( -name '*.tar.gz' -o -name '*.tar.gz.sha256' \) \
        -exec basename {} \; | sort)"
    want="$(pinned_files "${ctx}" | awk '{print; print $0 ".sha256"}' | sort)"
    if [ "${rc}" -eq 0 ] && [ "${have}" = "${want}" ]; then
        ok "${rel}/fetch.sh leaves only the pinned files in dl/"
    else
        bad "${rel}/fetch.sh: exit ${rc}; dl/ holds: $(echo "${have}" | tr '\n' ' ')"
    fi

    rc=0
    FIXTURE="${t}/fixture" PATH="${t}/bin:${PATH}" "${t}/ctx/fetch.sh" "${t}/outside" \
        > "${t}/log2" 2>&1 || rc=$?
    if [ "${rc}" -ne 0 ] && [ -f "${t}/outside/keep-me.tar.gz" ]; then
        ok "${rel}/fetch.sh refuses a directory other than its own dl/"
    else
        bad "${rel}/fetch.sh wrote to or cleared a directory other than its own dl/ (exit ${rc})"
    fi
}

n=0
for c in docker/sled docker/cloudtest; do
    n=$((n + 1))
    check_static "${root}/${c}"
    check_unpack "${root}/${c}" "${n}"
    check_fetch "${root}/${c}" "${n}"
done
if cmp -s "${root}/docker/sled/pinned-tarballs.sh" "${root}/docker/cloudtest/pinned-tarballs.sh"; then
    ok "docker/sled and docker/cloudtest pinned-tarballs.sh are identical"
else bad "docker/sled and docker/cloudtest pinned-tarballs.sh differ (or are missing)"; fi

echo "check_sled_pinned_tarballs: ${pass} passed, ${failed} failed"
[ "${failed}" -eq 0 ]
