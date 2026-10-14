#!/usr/bin/env bash
# provision-app-key.sh — provision a new GitHub App private key into
# AWS Secrets Manager for webapp-companion-app.
#
# Why: GitHub has no REST endpoint that creates an App private key. The one
# manual step is a human clicking "Generate a private key" on the App's
# settings page, which downloads a .pem. Everything else — finding the
# download, validating it, minting a JWT to prove it works against the
# GitHub API, and merging it into the target secret — is scripted so the
# manual step is the only place a person is needed.
#
# Usage:
#   provision-app-key.sh [--replace] [--open]
#   provision-app-key.sh --dry-run --pem <path>
#
#   --open      Actually run `open` on the App settings page and poll
#               ~/Downloads for a freshly generated key (real run only).
#               Without --open in a non-dry-run, the script still does
#               steps 2-5 but never shells out to `open`.
#   --replace   Allow overwriting an existing WEBAPP_COMPANION_APP_PRIVATE_KEY
#               in the target secret. Without it, an existing key refuses.
#   --dry-run   Run steps 1-3 (browser skipped) against a given --pem, then
#               steps 4-5 read-only: print the key names that would change,
#               never call put-secret-value, never delete the pem.
#   --pem PATH  Use this .pem instead of opening the browser and polling
#               Downloads: skips step 1 and step 2 entirely. Required with
#               --dry-run. In a real run it is validated, put, verified,
#               then rm -P'd on success (kept, path printed, on failure) —
#               same handling as a polled download. Use this when another
#               process (e.g. browser automation) already produced the .pem.
#
# Test: bash -n / shellcheck, then --dry-run against a throwaway RSA key.
# The GitHub /app call is expected to 401 with a throwaway key; the script
# must exit 3 cleanly and never print the key, JWT or token.
set -euo pipefail
umask 077

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------
readonly APP_ID="${GITHUB_APP_ID:?set GITHUB_APP_ID to the GitHub App id}"
readonly INSTALLATION_ID="${APP_INSTALLATION_ID:?set APP_INSTALLATION_ID to the GitHub App installation id}"
readonly APP_SETTINGS_URL="https://github.com/organizations/employer-org/settings/apps/webapp-companion-app"
readonly SECRET_ID="webapp-companion/production"
readonly AWS_REGION="us-east-1"
readonly AWS_PROFILE="cto"
readonly DOWNLOADS_DIR="${HOME}/Downloads"
readonly PEM_GLOB="webapp-companion-app*.private-key.pem"
readonly POLL_INTERVAL_S=2
readonly POLL_TIMEOUT_S=900 # 15 min

# ---------------------------------------------------------------------------
# Args
# ---------------------------------------------------------------------------
DRY_RUN=0
REPLACE=0
DO_OPEN=0
PEM_ARG=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run) DRY_RUN=1; shift ;;
    --replace) REPLACE=1; shift ;;
    --open) DO_OPEN=1; shift ;;
    --pem) PEM_ARG="${2:?--pem requires a path}"; shift 2 ;;
    -h|--help)
      sed -n '2,33p' "$0"
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if [[ "$DRY_RUN" -eq 1 && -z "$PEM_ARG" ]]; then
  echo "error: --dry-run requires --pem <path>" >&2
  exit 2
fi

# ---------------------------------------------------------------------------
# Temp files / cleanup
# ---------------------------------------------------------------------------
WORKDIR=""
PEM_PATH=""
PEM_IS_DOWNLOAD=0
PUT_SUCCEEDED=0

cleanup() {
  local ec=$?
  if [[ -n "$WORKDIR" && -d "$WORKDIR" ]]; then
    find "$WORKDIR" -type f -exec rm -P {} + 2>/dev/null || true
    rm -rf "$WORKDIR" 2>/dev/null || true
  fi
  if [[ "$PEM_IS_DOWNLOAD" -eq 1 && -n "$PEM_PATH" && -f "$PEM_PATH" ]]; then
    if [[ "$DRY_RUN" -eq 0 && "$PUT_SUCCEEDED" -eq 1 ]]; then
      rm -P "$PEM_PATH" 2>/dev/null || true
    else
      echo "downloaded key kept (not put/verified): $PEM_PATH" >&2
    fi
  fi
  exit "$ec"
}
trap cleanup EXIT

WORKDIR="$(mktemp -d)"

# ---------------------------------------------------------------------------
# Step 1: kick off the manual step
# ---------------------------------------------------------------------------
step1() {
  local start_epoch
  start_epoch="$(date -u +%s)"
  if [[ "$DRY_RUN" -eq 0 && "$DO_OPEN" -eq 1 ]]; then
    open "$APP_SETTINGS_URL"
  fi
  echo "the owner: on the App settings page, click \"Generate a private key\" now." >&2
  echo "$start_epoch"
}

# ---------------------------------------------------------------------------
# Step 2: poll Downloads for a new matching pem
# ---------------------------------------------------------------------------
find_new_pem() {
  local since_epoch="$1"
  local deadline candidate newest_mtime newest_file mtime
  deadline=$(( $(date -u +%s) + POLL_TIMEOUT_S ))
  while true; do
    newest_mtime=0
    newest_file=""
    while IFS= read -r -d '' candidate; do
      mtime="$(stat -f %m "$candidate" 2>/dev/null || echo 0)"
      if [[ "$mtime" -gt "$since_epoch" && "$mtime" -gt "$newest_mtime" ]]; then
        newest_mtime="$mtime"
        newest_file="$candidate"
      fi
    done < <(find "$DOWNLOADS_DIR" -maxdepth 1 -type f -name "$PEM_GLOB" -print0 2>/dev/null)
    if [[ -n "$newest_file" ]]; then
      echo "$newest_file"
      return 0
    fi
    if [[ "$(date -u +%s)" -ge "$deadline" ]]; then
      return 1
    fi
    sleep "$POLL_INTERVAL_S"
  done
}

# ---------------------------------------------------------------------------
# Step 3: validate the key, mint a JWT, hit the GitHub API
# ---------------------------------------------------------------------------
b64url() {
  # stdin -> base64url, no padding, no newline
  openssl base64 -A | tr '+/' '-_' | tr -d '='
}

validate_and_check() {
  local pem="$1"

  # 1. structural check, no output printed
  if ! openssl rsa -in "$pem" -check -noout >/dev/null 2>"$WORKDIR/rsa-check.err"; then
    echo "error: openssl rsa -check failed on $pem" >&2
    return 3
  fi

  # 2. mint RS256 JWT
  local iat exp header payload signing_input sig jwt header_b64 payload_b64
  iat=$(( $(date -u +%s) - 60 ))
  exp=$(( $(date -u +%s) + 540 ))
  header='{"alg":"RS256","typ":"JWT"}'
  payload=$(jq -nc --arg iat "$iat" --arg exp "$exp" --arg iss "$APP_ID" \
    '{iat: ($iat|tonumber), exp: ($exp|tonumber), iss: $iss}')

  header_b64="$(printf '%s' "$header" | b64url)"
  payload_b64="$(printf '%s' "$payload" | b64url)"
  signing_input="${header_b64}.${payload_b64}"

  sig="$(printf '%s' "$signing_input" | openssl dgst -sha256 -sign "$pem" | b64url)"
  jwt="${signing_input}.${sig}"

  # 3. GET /app — print only .id and .slug, never the token
  local app_resp app_id app_slug
  app_resp="$(curl -s -H "Authorization: Bearer ${jwt}" -H "Accept: application/vnd.github+json" \
    https://api.github.com/app)"
  app_id="$(printf '%s' "$app_resp" | jq -r '.id // empty' 2>/dev/null || true)"
  app_slug="$(printf '%s' "$app_resp" | jq -r '.slug // empty' 2>/dev/null || true)"

  echo "app id: ${app_id:-<none>}"
  echo "app slug: ${app_slug:-<none>}"

  if [[ "$app_id" != "$APP_ID" ]]; then
    echo "error: /app returned id '${app_id:-<empty>}', expected ${APP_ID}" >&2
    return 3
  fi

  # 4. GET /app/installations/<id> — require .id match, print nothing else
  local inst_resp inst_id
  inst_resp="$(curl -s -H "Authorization: Bearer ${jwt}" -H "Accept: application/vnd.github+json" \
    "https://api.github.com/app/installations/${INSTALLATION_ID}")"
  inst_id="$(printf '%s' "$inst_resp" | jq -r '.id // empty' 2>/dev/null || true)"

  if [[ "$inst_id" != "$INSTALLATION_ID" ]]; then
    echo "error: /app/installations/${INSTALLATION_ID} returned id '${inst_id:-<empty>}'" >&2
    return 3
  fi

  echo "installation id confirmed: ${inst_id}"
  return 0
}

# ---------------------------------------------------------------------------
# Step 4/5: merge into Secrets Manager and verify
# ---------------------------------------------------------------------------
merge_secret() {
  local pem="$1"
  local cur="$WORKDIR/current.json"
  local new="$WORKDIR/new.json"
  ( umask 077; : > "$cur"; : > "$new" )

  aws secretsmanager get-secret-value \
    --secret-id "$SECRET_ID" \
    --region "$AWS_REGION" \
    --profile "$AWS_PROFILE" \
    --query SecretString --output text > "$cur"

  if jq -e 'has("WEBAPP_COMPANION_APP_PRIVATE_KEY")' "$cur" >/dev/null 2>&1; then
    if [[ "$REPLACE" -ne 1 ]]; then
      echo "error: WEBAPP_COMPANION_APP_PRIVATE_KEY already exists in ${SECRET_ID}; pass --replace to overwrite" >&2
      return 4
    fi
  fi

  local before_count
  before_count="$(jq 'keys | length' "$cur")"

  jq -c --rawfile k "$pem" --arg app_id "$APP_ID" --arg inst_id "$INSTALLATION_ID" \
    '. + {WEBAPP_COMPANION_APP_PRIVATE_KEY: ($k|rtrimstr("\n")|gsub("\n";"\\n")), WEBAPP_COMPANION_APP_ID: $app_id, WEBAPP_COMPANION_APP_INSTALLATION_ID: $inst_id}' \
    "$cur" > "$new"

  echo "keys before: ${before_count}"
  echo "keys that would be present after merge:"
  jq -r 'keys | sort | .[]' "$new"

  if [[ "$DRY_RUN" -eq 1 ]]; then
    echo "dry-run: no put-secret-value performed"
    return 0
  fi

  aws secretsmanager put-secret-value \
    --secret-id "$SECRET_ID" \
    --region "$AWS_REGION" \
    --profile "$AWS_PROFILE" \
    --secret-string "file://${new}" >/dev/null

  PUT_SUCCEEDED=1

  # Step 5: re-read and verify
  local verify="$WORKDIR/verify.json"
  aws secretsmanager get-secret-value \
    --secret-id "$SECRET_ID" \
    --region "$AWS_REGION" \
    --profile "$AWS_PROFILE" \
    --query SecretString --output text > "$verify"

  local verify_count
  verify_count="$(jq 'keys | length' "$verify")"

  echo "keys after put (verified):"
  jq -r 'keys | sort | .[]' "$verify"

  local expected_count=$(( before_count + 3 ))
  if [[ "$verify_count" -ne "$expected_count" ]]; then
    echo "error: expected ${expected_count} keys after merge, verified ${verify_count}" >&2
    PUT_SUCCEEDED=0
    return 4
  fi

  for k in WEBAPP_COMPANION_APP_PRIVATE_KEY WEBAPP_COMPANION_APP_ID WEBAPP_COMPANION_APP_INSTALLATION_ID; do
    if ! jq -e --arg k "$k" 'has($k)' "$verify" >/dev/null; then
      echo "error: verified secret is missing ${k}" >&2
      PUT_SUCCEEDED=0
      return 4
    fi
  done

  echo "verify ok: ${SECRET_ID} has ${verify_count} keys (was ${before_count})"
  return 0
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
main() {
  if [[ -n "$PEM_ARG" ]]; then
    # --pem supplied: skip step 1 (open + manual click) and step 2 (Downloads
    # poll) entirely — the caller already has the key on disk (e.g. the
    # supervisor's own browser automation). Validate, put, verify, then
    # rm -P it on success, same as a polled download. In --dry-run this
    # never deletes (cleanup() gates deletion on DRY_RUN==0).
    PEM_PATH="$PEM_ARG"
    PEM_IS_DOWNLOAD=1
  else
    local since_epoch
    since_epoch="$(step1)"
    echo "polling ${DOWNLOADS_DIR} for ${PEM_GLOB} (up to $((POLL_TIMEOUT_S/60)) min)..." >&2
    if ! PEM_PATH="$(find_new_pem "$since_epoch")"; then
      echo "error: timed out waiting for a new ${PEM_GLOB} in ${DOWNLOADS_DIR}" >&2
      exit 2
    fi
    PEM_IS_DOWNLOAD=1
    echo "found: $PEM_PATH" >&2
  fi

  if ! validate_and_check "$PEM_PATH"; then
    exit 3
  fi

  merge_secret "$PEM_PATH"
}

main "$@"
