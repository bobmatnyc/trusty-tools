#!/bin/sh
# Regenerate the trusty-browser parity fixtures from Python 0.7.0.
# Usage: capture.sh <venv-python> <out-dir>
# A throwaway keychain is created in a temp dir, named explicitly on every
# `security` call, and deleted on exit. The login keychain and the default or
# search keychain lists are never read or changed.
set -eu
PY=$1
OUT=$2
TMP=$(mktemp -d /tmp/tb-capture-kc.XXXXXX)
KC="$TMP/capture.keychain-db"
PW=$(openssl rand -hex 16)
cleanup() {
    security delete-keychain "$KC" 2>/dev/null || true
    rm -rf "$TMP"
}
trap cleanup EXIT INT TERM
security create-keychain -p "$PW" "$KC"
security unlock-keychain -p "$PW" "$KC"
security set-keychain-settings "$KC"   # no lock timeout, no lock on sleep
mkdir -p "$TMP/home"
HOME="$TMP/home" TB_CAPTURE_KEYCHAIN="$KC" "$PY" "$(dirname "$0")/capture.py" --out "$OUT" </dev/null
HOME="$TMP/home" TB_CAPTURE_KEYCHAIN="$KC" "$PY" "$(dirname "$0")/capture_pages.py" --out "$OUT" </dev/null
