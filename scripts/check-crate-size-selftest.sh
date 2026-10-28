#!/usr/bin/env bash
#
# check-crate-size-selftest.sh — failing-case fixtures for
# scripts/check-crate-size.sh.
#
# Why: the 1.9.0 publish hit a 413 that no gate predicted. A size gate that
#   only ever ran against a small crate would look the same as one that never
#   compares anything, so each branch (FAIL, WARN, PASS, fail-closed) runs here.
#
# What: puts a stub `cargo` first on PATH. The stub writes a `.crate` of a chosen
#   size under `${CARGO_TARGET_DIR}/package/`, so the whole flow runs: stale-file
#   removal, CARGO_TARGET_DIR-aware lookup, measurement, verdict. No real cargo.
#
# Test: this IS the test. Run directly:
#   bash scripts/check-crate-size-selftest.sh
#
# Portability: bash 3.2 and bash 5, POSIX tools only.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="${SCRIPT_DIR}/check-crate-size.sh"
LIMIT=10485760

WORK="$(mktemp -d "${TMPDIR:-/tmp}/check-crate-size-selftest.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "${WORK}/bin" "${WORK}/repo"

# Stub cargo: STUB_CRATE_BYTES=<n> writes an n-byte archive; STUB_CARGO_RC=<n>
# fails; STUB_CRATE_BYTES unset exits 0 and writes nothing.
cat > "${WORK}/bin/cargo" <<'STUB'
#!/usr/bin/env bash
[ "${STUB_CARGO_RC:-0}" -eq 0 ] || { echo "stub cargo: forced failure" >&2; exit "$STUB_CARGO_RC"; }
if [ -n "${STUB_CRATE_BYTES:-}" ]; then
  d="${CARGO_TARGET_DIR:-$PWD/target}/package"
  mkdir -p "$d"
  head -c "$STUB_CRATE_BYTES" /dev/zero > "$d/demo-1.0.0.crate"
fi
STUB
chmod +x "${WORK}/bin/cargo"

PASSED=0
FAILED=0

# run_case <label> <expect-exit> <expect-substring> [env VAR=val ...]
run_case() {
  local label="$1" want_rc="$2" want_sub="$3"; shift 3
  local out rc=0
  out="$(env PATH="${WORK}/bin:${PATH}" CRATE_SIZE_REPO_ROOT="${WORK}/repo" \
    CARGO_TARGET_DIR="${WORK}/tgt" "$@" bash "$GATE" demo 1.0.0 2>&1)" || rc=$?
  if [ "$rc" -eq "$want_rc" ] && printf '%s' "$out" | grep -qF -- "$want_sub"; then
    echo "  ok  ${label}"; PASSED=$((PASSED + 1))
  else
    echo "SELF-TEST FAIL: ${label} (rc=${rc}, wanted ${want_rc} + '${want_sub}')" >&2
    printf '%s\n' "$out" | sed 's/^/       /' >&2
    FAILED=$((FAILED + 1))
  fi
}

run_case "small crate passes"                0 "[PASS] crate-size" STUB_CRATE_BYTES=1000
run_case "exactly the limit fails (>=)"      1 "[FAIL] crate-size" STUB_CRATE_BYTES="$LIMIT"
run_case "over the limit fails, names size and limit" 1 "$((LIMIT + 1)) B packaged" STUB_CRATE_BYTES="$((LIMIT + 1))"
run_case "limit is named in the FAIL line"   1 "limit ${LIMIT} B" STUB_CRATE_BYTES="$((LIMIT + 1))"
run_case "one byte under the limit warns"    0 "[WARN] crate-size" STUB_CRATE_BYTES="$((LIMIT - 1))"
run_case "exactly 90% warns"                 0 "[WARN] crate-size" STUB_CRATE_BYTES=9437184
run_case "just under 90% passes"             0 "[PASS] crate-size" STUB_CRATE_BYTES=9437183
run_case "no .crate produced is a FAIL"      1 "[FAIL] crate-size: no .crate" STUB_CRATE_BYTES=
run_case "cargo package failure is a FAIL"   1 "exited 3" STUB_CARGO_RC=3 STUB_CRATE_BYTES=1000

# A stale oversize archive must not decide the verdict when packaging writes nothing.
mkdir -p "${WORK}/tgt/package"
head -c 100 /dev/zero > "${WORK}/tgt/package/demo-1.0.0.crate"
run_case "stale small .crate is not reused"  1 "[FAIL] crate-size: no .crate" STUB_CRATE_BYTES=

# --crate-file measures an existing archive without calling cargo.
head -c "$((LIMIT + 5))" /dev/zero > "${WORK}/big.crate"
rc=0
out="$(PATH="${WORK}/bin:${PATH}" bash "$GATE" --crate-file "${WORK}/big.crate" demo 1.0.0 2>&1)" || rc=$?
if [ "$rc" -eq 1 ] && printf '%s' "$out" | grep -qF "[FAIL] crate-size"; then
  echo "  ok  --crate-file over limit fails"; PASSED=$((PASSED + 1))
else
  echo "SELF-TEST FAIL: --crate-file over limit (rc=${rc})" >&2; FAILED=$((FAILED + 1))
fi

echo "check-crate-size-selftest: ${PASSED} passed, ${FAILED} failed"
[ "$FAILED" -eq 0 ]
