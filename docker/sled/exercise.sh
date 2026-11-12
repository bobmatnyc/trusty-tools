#!/usr/bin/env bash
# exercise.sh [--tailscale-login] — the DOC-90 section 9 V1 standard exercise,
# run inside the app container after `check.sh`:
#   docker exec -u sled tm-trusty-cloud-sled exercise.sh
# Steps: token-file modes (stat only), one Claude Code session, one git fetch
# and one gh api call on SLED_REPO (default bobmatnyc/trusty-tools). With
# --tailscale-login it also asks tailscaled for a login URL, which reaches the
# control plane; the URL is never printed. Raw output: /data/logs/exercise-<ts>.log.
# Exits non-zero if a step that must work FAILs. Refs #11073
set -uo pipefail

REPO="${SLED_REPO:-bobmatnyc/trusty-tools}"
TS="$(date -u +%Y%m%dT%H%M%SZ)"
LOG="/data/logs/exercise-${TS}.log"
mkdir -p /data/logs /data/work
: > "${LOG}"
FAILS=0
# A tailscale login URL is a live login link: every https URL is redacted.
redact() { sed -E 's#https://[^ ]+#<url redacted>#g'; }

# run NAME MUST CMD... — run CMD with a 180 s bound and log its output,
# redacted. MUST=yes makes a non-zero exit a FAIL; MUST=no reports it as INFO.
run() {
    local name="$1" must="$2" out rc=0
    shift 2
    out="$(timeout 180 "$@" 2>&1)" || rc=$?
    { echo "=== ${name} (rc=${rc}) ==="; printf '%s\n' "${out}" | redact; } >> "${LOG}"
    if [ "${rc}" -eq 0 ]; then
        echo "PASS ${name}"
    elif [ "${must}" = yes ]; then
        echo "FAIL ${name} (rc=${rc}); see ${LOG}"
        FAILS=$((FAILS + 1))
    else
        echo "INFO ${name} (rc=${rc}); see ${LOG}"
    fi
}

# Token files: mode and owner by stat only, never read here (DOC-90 TOK-2, V3).
for f in /run/secrets/github-token /run/secrets/trusty-console-token; do
    if [ ! -e "${f}" ]; then
        echo "FAIL token file ${f} absent"; FAILS=$((FAILS + 1)); continue
    fi
    m="$(stat -c '%a %U' "${f}")"
    case "${m}" in
        "400 sled"|"600 sled") echo "PASS token file ${f} mode ${m}" ;;
        *) echo "FAIL token file ${f} mode ${m} (want 400 or 600, owner sled)"; FAILS=$((FAILS + 1)) ;;
    esac
done

# One Claude Code session. Without a stored login it still runs, so V1 sees
# every host it tries; the result is then INFO, not FAIL. Claude Code with no
# login opens no connection, so a credential-free request to the model API then
# proves the steady path (Anthropic answers 401; nothing secret is sent).
if test -s /data/home/.claude/.credentials.json || test -s /run/secrets/claude.env; then
    run "claude -p session" yes claude-run -p "Reply with the single word READY" --output-format json
else
    run "claude -p session (no login stored)" no claude-run -p "Reply with the single word READY" --output-format json
    run "api.anthropic.com through the proxy (no credential)" yes \
        curl -sS -m 20 -o /dev/null -w '%{http_code}\n' https://api.anthropic.com/v1/models
fi

# One git fetch: commits and trees of the default branch tip, no blobs.
FETCH_DIR="/data/work/fetch-probe"
[ -d "${FETCH_DIR}/.git" ] || git init -q "${FETCH_DIR}"
run "git fetch ${REPO}" yes gh-token-run git -C "${FETCH_DIR}" fetch --depth 1 \
    --filter=blob:none "https://github.com/${REPO}" HEAD

# One gh api call; prints the repository name only.
run "gh api repos/${REPO}" yes gh-token-run gh api "repos/${REPO}" --jq .full_name

if [ "${1:-}" = "--tailscale-login" ]; then
    # `tailscale up` with no auth key asks the control plane for a login URL.
    # The URL is a live login link: it goes to the log redacted, never stdout.
    out="$(timeout 60 tailscale up --timeout=45s 2>&1)"; rc=$?
    { echo "=== tailscale up (rc=${rc}) ==="; printf '%s\n' "${out}" | redact; } >> "${LOG}"
    if grep -q 'https://login.tailscale.com/' <<< "${out}"; then
        echo "PASS tailscale up reached the control plane (login URL issued, not printed)"
    else
        echo "INFO tailscale up issued no login URL (rc=${rc}); see ${LOG}"
    fi
    run "tailscale status" no tailscale status
fi

echo "log: ${LOG}"
if [ "${FAILS}" -gt 0 ]; then
    echo "RESULT: ${FAILS} step(s) FAILED"
    exit 1
fi
echo "RESULT: exercise done; run check_v1.sh on the host"
