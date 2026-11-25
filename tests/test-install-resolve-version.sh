#!/usr/bin/env bash
# tests/test-install-resolve-version.sh — unit tests for install.sh's
# resolve_version (#11723).
#
# Why: the monorepo publishes many crates, so the newest trusty-installer-v*
# tag can fall outside GitHub's default first page of 30 releases. An
# unpaginated lookup then died with "no release found" and bootstrap never
# completed.
#
# What: sources install.sh with TRUSTY_INSTALL_SOURCE_ONLY=1 and puts a stub
# `curl` first on PATH. The stub serves a fixture paginated by the request's
# `per_page`/`page` query (default 30 per page, page 1), so no network is used.
# - installer tag only on page 2 of per_page=100 results: resolved
# - installer tag absent from the first 30 but inside the first 100: resolved
# - no installer tag on any page: non-zero exit, clear message, nothing on stdout
# - HTTP 500 and HTTP 200 with an empty body: non-zero exit, nothing on stdout
# - TRUSTY_VERSION pins without calling curl
#
# Test: This file *is* the test. Run with: tests/test-install-resolve-version.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_SH="$(cd "$SCRIPT_DIR/.." && pwd)/install.sh"

PASS=0
FAIL=0

check() {
    local ok="$1" desc="$2" detail="${3:-}"
    if [[ "$ok" == "1" ]]; then
        echo "  PASS: $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $desc"
        [[ -n "$detail" ]] && echo "    $detail"
        FAIL=$((FAIL + 1))
    fi
}

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/bin"

# Stub curl. Env: FIXTURE_TAGS (file, one tag per line), STUB_MODE
# (ok | http500 | empty), CURL_LOG (file; one URL per call).
cat >"$WORK/bin/curl" <<'STUB'
#!/usr/bin/env bash
out="" url=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        -o) out="$2"; shift 2 ;;
        -w | -H) shift 2 ;;
        -*) shift ;;
        *) url="$1"; shift ;;
    esac
done
echo "$url" >>"$CURL_LOG"
case "$STUB_MODE" in
    http500) : >"$out"; printf '500'; exit 0 ;;
    empty) : >"$out"; printf '200'; exit 0 ;;
esac
per=30 page=1
q="${url#*\?}"
[ "$q" != "$url" ] || q=""
IFS='&' read -ra kv <<<"$q"
for p in "${kv[@]:-}"; do
    case "$p" in
        per_page=*) per="${p#per_page=}" ;;
        page=*) page="${p#page=}" ;;
    esac
done
start=$(((page - 1) * per + 1))
end=$((page * per))
sed -n "${start},${end}p" "$FIXTURE_TAGS" | while read -r t; do
    printf '  {\n    "tag_name": "%s",\n    "draft": false\n  },\n' "$t"
done >"$out"
printf '200'
STUB
chmod +x "$WORK/bin/curl"

# gen_tags <other-count> <installer-position|0>: other-count filler tags, the
# installer tag inserted at 1-based position (0 = none).
gen_tags() {
    local n="$1" pos="$2" i
    for ((i = 1; i <= n; i++)); do
        if [[ "$i" == "$pos" ]]; then echo "trusty-installer-v0.17.1"; fi
        echo "trusty-mpm-v1.0.$i"
    done
}

# run_resolve <mode> [TRUSTY_VERSION]: prints "<exit>|<stdout>|<stderr>" parts
# into the globals RC / OUT / ERR.
run_resolve() {
    local mode="$1" pin="${2:-}"
    : >"$CURL_LOG"
    OUT="$(
        PATH="$WORK/bin:$PATH" STUB_MODE="$mode" FIXTURE_TAGS="$WORK/tags" \
            CURL_LOG="$CURL_LOG" TRUSTY_VERSION="$pin" TRUSTY_INSTALL_SOURCE_ONLY=1 \
            GITHUB_TOKEN="" GH_TOKEN="" \
            bash -c '. "$1"; resolve_version' _ "$INSTALL_SH" 2>"$WORK/stderr"
    )"
    RC=$?
    ERR="$(cat "$WORK/stderr")"
}
CURL_LOG="$WORK/curl.log"

echo "Case 1: installer tag only on page 2 of per_page=100"
gen_tags 150 120 >"$WORK/tags"
run_resolve ok
check "$([[ $RC == 0 && $OUT == "0.17.1" ]] && echo 1 || echo 0)" \
    "resolves 0.17.1 from page 2" "rc=$RC out='$OUT' err='$ERR'"

echo "Case 2: installer tag at position 60 (beyond default 30, inside 100)"
gen_tags 90 60 >"$WORK/tags"
run_resolve ok
check "$([[ $RC == 0 && $OUT == "0.17.1" ]] && echo 1 || echo 0)" \
    "resolves 0.17.1 from the first 100" "rc=$RC out='$OUT' err='$ERR'"

echo "Case 3: no installer tag on any page"
gen_tags 150 0 >"$WORK/tags"
run_resolve ok
check "$([[ $RC != 0 && -z $OUT && $ERR == *"no trusty-installer-v* release found"* ]] && echo 1 || echo 0)" \
    "non-zero exit, clear message, empty stdout" "rc=$RC out='$OUT' err='$ERR'"

echo "Case 4: HTTP 500 on every attempt"
run_resolve http500
check "$([[ $RC != 0 && -z $OUT && $ERR == *"HTTP 500"* ]] && echo 1 || echo 0)" \
    "non-zero exit naming HTTP 500" "rc=$RC out='$OUT' err='$ERR'"

echo "Case 5: HTTP 200 with an empty body"
run_resolve empty
check "$([[ $RC != 0 && -z $OUT && $ERR == *"no trusty-installer-v* release found"* ]] && echo 1 || echo 0)" \
    "non-zero exit, no version invented" "rc=$RC out='$OUT' err='$ERR'"

echo "Case 6: TRUSTY_VERSION pins without network"
gen_tags 150 0 >"$WORK/tags"
run_resolve ok 0.9.9
check "$([[ $RC == 0 && $OUT == "0.9.9" && ! -s $CURL_LOG ]] && echo 1 || echo 0)" \
    "pinned 0.9.9, curl never called" "rc=$RC out='$OUT'"

echo
echo "passed=$PASS failed=$FAIL"
[[ "$FAIL" == 0 ]]
