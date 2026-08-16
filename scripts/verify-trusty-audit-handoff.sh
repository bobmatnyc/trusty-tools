#!/usr/bin/env bash
#
# verify-trusty-audit-handoff.sh — open the trusty-audit handoff zip and
# check its actual contents, shaped for issue #5483's REAL target layout.
#
# Why: two failure shapes this repo has been bitten by, both guarded against
#   here:
#
#   #5620 — "found nothing" must never report success. `check_semver.sh`
#     exited 0 both when it compared a crate and when it compared NOTHING, so
#     `0 crate(s) checked` printed `[PASS]`. This script distinguishes
#     "verified and correct" from "nothing found to verify" — an absent,
#     empty, or unreadable zip is a FAIL, never a pass by default (see
#     summarize_and_exit below: zero checks run can never exit 0).
#
#   #3540 — a false `if:` silently SKIPS a job, and a skipped release is
#     silent. This script is deliberately NOT wired into any CI workflow: as
#     of its introduction there is no build job that produces a real `.app`
#     (#5477/#5478/#5484 are all unimplemented — see
#     package-trusty-audit-handoff.sh's header), so a CI job calling this
#     today would always find nothing and either always fail loud (useless
#     noise on every PR) or be given an `if:` to skip when there's nothing to
#     check (exactly the #3540 shape, from the other direction). Wire it in
#     once a real `.app`-producing job exists, as a `needs:` step that fails
#     BEFORE any upload — not an `if:` on the job (see PR #5767's pattern for
#     the release-completeness audit).
#
# 🔴 THIS DOES NOT CLOSE #5483. Same waits-on list as the packaging script:
#   #5477 (client app), #5478 (config schema/generator), #5484 (signing /
#   notarization).
#
# What: given a zip built to package-trusty-audit-handoff.sh's canonical
#   layout, verifies:
#
#   1. the zip exists, is non-empty, and is readable as a zip archive.
#   2. exactly three top-level members: one `*.app` bundle, one literal
#      `README.md`, and exactly one other regular file (the config) — a
#      missing OR an extra/unexpected member is a FAIL.
#   3. the `.app` has a real bundle shape (Contents/Info.plist,
#      Contents/MacOS/<executable>), and that executable is identified by
#      `file`(1) as a Mach-O 64-bit **executable**, arm64 — never x86_64-only,
#      never a truncated/corrupt partial header.
#   4. if --expect-version is given AND the host CPU itself is arm64, the
#      executable is actually RUN with `--version` and its output is checked
#      for the expected substring — never assumed from the static check
#      alone. On a non-arm64 host this check is explicitly reported as
#      skipped ("not verified here"), never silently treated as passed.
#   5. the config member parses as TOML (python3's `tomllib`) — a
#      byte-present but unparseable file is a FAIL.
#   6. signing / notarization state is REPORTED via `codesign`/`spctl`,
#      always printed, separately from checks 2-5. #5484 does not exist yet,
#      so nothing packaged today will ever pass a signing check — this is
#      informational by default (pass `--require-signed` to make it a hard
#      gate once #5484 lands). When codesign/spctl themselves are unavailable
#      in the running environment, that is reported explicitly as
#      "not verified here" — it is never allowed to read as "signed".
#
# Usage:
#   scripts/verify-trusty-audit-handoff.sh --zip <FILE.zip> \
#     [--expect-version <STRING>] [--require-signed]
#
# Exit: 0 only when every hard check (2-5) ran AND passed. 1 on any failure,
#   including an absent/empty/unreadable zip (zero checks run is itself a
#   FAIL — see summarize_and_exit). 2 on a usage error.
#
# Test: scripts/verify-trusty-audit-handoff-selftest.sh

set -uo pipefail  # deliberately not -e: we want to accumulate every failing
                   # check and report all of them, not stop at the first one

usage() {
  cat >&2 <<'EOF'
Usage: scripts/verify-trusty-audit-handoff.sh --zip <FILE.zip> [--expect-version <STRING>] [--require-signed]
EOF
}

ZIP=""
EXPECT_VERSION=""
REQUIRE_SIGNED=""

while [ $# -gt 0 ]; do
  case "$1" in
    --zip) ZIP="${2:-}"; shift 2 ;;
    --expect-version) EXPECT_VERSION="${2:-}"; shift 2 ;;
    --require-signed) REQUIRE_SIGNED=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "FAIL: unknown argument: $1" >&2; usage; exit 2 ;;
  esac
done

if [ -z "$ZIP" ]; then
  echo "FAIL: --zip is required" >&2
  usage
  exit 2
fi

FAILURES=0
CHECKS=0

fail() { printf '[FAIL] %s\n' "$1" >&2; FAILURES=$((FAILURES + 1)); }
ok()   { printf '[ok]   %s\n' "$1"; CHECKS=$((CHECKS + 1)); }
info() { printf '[INFO] %s\n' "$1"; }

summarize_and_exit() {
  echo ""
  # Belt-and-suspenders against the #5620 shape: even if every individual
  # check above were somehow bypassed, zero checks run is a FAIL, never the
  # silent PASS that "0 compared" produced there.
  if [ "$CHECKS" -eq 0 ] && [ "$FAILURES" -eq 0 ]; then
    fail "no checks were run — nothing was verified"
  fi
  if [ "$FAILURES" -gt 0 ]; then
    printf 'VERIFY: FAIL — %d check(s) passed, %d check(s) failed\n' "$CHECKS" "$FAILURES"
    exit 1
  fi
  printf 'VERIFY: PASS — %d check(s) passed, 0 failed\n' "$CHECKS"
  exit 0
}

# --- 1. the zip itself --------------------------------------------------------

if [ ! -e "$ZIP" ]; then
  fail "zip does not exist: $ZIP"
  summarize_and_exit
fi
if [ ! -s "$ZIP" ]; then
  fail "zip is empty (0 bytes): $ZIP"
  summarize_and_exit
fi
if ! LISTING="$(unzip -l "$ZIP" 2>&1)"; then
  fail "zip is not a readable archive: $ZIP"
  printf '%s\n' "$LISTING" | sed 's/^/       /' >&2
  summarize_and_exit
fi
ok "zip exists, is non-empty, and is a readable archive: $ZIP"

ENTRIES="$(unzip -Z1 "$ZIP" 2>/dev/null || true)"
if [ -z "$ENTRIES" ]; then
  fail "zip contains zero entries — nothing to verify"
  summarize_and_exit
fi
if printf '%s\n' "$ENTRIES" | grep -q '\.\./'; then
  fail "zip contains a path-traversal entry (../) — refusing to extract"
  summarize_and_exit
fi

# --- 2. exactly three top-level members --------------------------------------

TOP_NAMES="$(printf '%s\n' "$ENTRIES" | awk -F/ '{print $1}' | sort -u)"

APP_NAMES="$(printf '%s\n' "$TOP_NAMES" | grep '\.app$' || true)"
N_APP=$(printf '%s\n' "$APP_NAMES" | grep -c . || true)
if [ "$N_APP" -ne 1 ]; then
  fail "expected exactly one top-level *.app member, found $N_APP: $(printf '%s' "$APP_NAMES" | tr '\n' ' ')"
  APP_NAME=""
else
  APP_NAME="$(printf '%s\n' "$APP_NAMES" | head -1)"
  ok "exactly one top-level .app member: $APP_NAME"
fi

if printf '%s\n' "$TOP_NAMES" | grep -qx "README.md"; then
  ok "top-level README.md present"
else
  fail "missing top-level README.md"
fi

REMAINING="$(printf '%s\n' "$TOP_NAMES" | grep -vx "README.md" | grep -v '\.app$' || true)"
N_REMAINING=$(printf '%s\n' "$REMAINING" | grep -c . || true)
if [ "$N_REMAINING" -ne 1 ]; then
  fail "expected exactly one top-level config file, found $N_REMAINING: $(printf '%s' "$REMAINING" | tr '\n' ' ')"
  CONFIG_NAME=""
else
  CONFIG_NAME="$(printf '%s\n' "$REMAINING" | head -1)"
  if printf '%s\n' "$ENTRIES" | grep -q "^${CONFIG_NAME}/"; then
    fail "$CONFIG_NAME looks like a directory inside the zip, expected a single file"
    CONFIG_NAME=""
  else
    ok "exactly one top-level config member: $CONFIG_NAME"
  fi
fi

# Nothing further to inspect without an app to extract.
if [ -z "$APP_NAME" ]; then
  summarize_and_exit
fi

# --- extract for inspection ---------------------------------------------------

TMP_EXTRACT="$(mktemp -d)"
trap 'rm -rf "$TMP_EXTRACT"' EXIT
if ! EXTRACT_ERR="$(unzip -q "$ZIP" -d "$TMP_EXTRACT" 2>&1)"; then
  fail "failed to extract zip for inspection: $ZIP"
  printf '%s\n' "$EXTRACT_ERR" | sed 's/^/       /' >&2
  summarize_and_exit
fi

APP_DIR="$TMP_EXTRACT/$APP_NAME"

# --- 3. app bundle shape + architecture ---------------------------------------

if [ -f "$APP_DIR/Contents/Info.plist" ]; then
  ok "$APP_NAME: Contents/Info.plist present"
else
  fail "$APP_NAME: missing Contents/Info.plist (not a real bundle shape)"
fi

if [ -d "$APP_DIR/Contents/MacOS" ]; then
  ok "$APP_NAME: Contents/MacOS/ present"
else
  fail "$APP_NAME: missing Contents/MacOS/ (not a real bundle shape)"
fi

EXEC_NAME=""
if [ -f "$APP_DIR/Contents/Info.plist" ] && command -v plutil >/dev/null 2>&1; then
  EXEC_NAME="$(plutil -extract CFBundleExecutable raw -o - "$APP_DIR/Contents/Info.plist" 2>/dev/null || true)"
fi
if [ -z "$EXEC_NAME" ] && [ -d "$APP_DIR/Contents/MacOS" ]; then
  EXEC_NAME="$(ls -1 "$APP_DIR/Contents/MacOS" 2>/dev/null | head -1)"
fi

EXEC_PATH=""
FILE_OUT=""
if [ -z "$EXEC_NAME" ]; then
  fail "$APP_NAME: no executable found under Contents/MacOS/ (and CFBundleExecutable did not resolve one)"
else
  EXEC_PATH="$APP_DIR/Contents/MacOS/$EXEC_NAME"
  if [ ! -f "$EXEC_PATH" ]; then
    fail "$APP_NAME: CFBundleExecutable names '$EXEC_NAME' but it does not exist at Contents/MacOS/$EXEC_NAME"
    EXEC_PATH=""
  else
    FILE_OUT="$(file -b "$EXEC_PATH" 2>&1)"
    case "$FILE_OUT" in
      *"Mach-O"*"executable"*arm64*)
        ok "$APP_NAME executable ($EXEC_NAME) is a Mach-O arm64 executable: $FILE_OUT" ;;
      *"Mach-O"*"executable"*)
        fail "$APP_NAME executable ($EXEC_NAME) is NOT arm64 (wrong architecture): $FILE_OUT" ;;
      *"Mach-O"*)
        fail "$APP_NAME executable ($EXEC_NAME) is corrupt or truncated (Mach-O header present but incomplete): $FILE_OUT" ;;
      *)
        fail "$APP_NAME executable ($EXEC_NAME) is not a valid Mach-O executable, possibly corrupt or truncated: $FILE_OUT" ;;
    esac
  fi
fi

# --- 4. --version execution check ---------------------------------------------

if [ -z "$EXPECT_VERSION" ]; then
  info "--expect-version not given: --version execution check skipped"
elif [ -z "$EXEC_PATH" ]; then
  fail "cannot run --version: no valid executable resolved above"
else
  HOST_ARCH="$(uname -m)"
  if [ "$HOST_ARCH" != "arm64" ]; then
    info "--version execution check: not verified here (host is $HOST_ARCH, not arm64)"
  elif ! printf '%s' "$FILE_OUT" | grep -q "executable.*arm64\|arm64.*executable"; then
    info "--version execution check: not verified here (executable did not pass the arm64 check above)"
  elif [ ! -x "$EXEC_PATH" ]; then
    fail "$APP_NAME executable ($EXEC_NAME) is not marked executable — cannot run --version"
  else
    VERSION_OUT="$("$EXEC_PATH" --version 2>&1)" && VERSION_RC=0 || VERSION_RC=$?
    if [ "$VERSION_RC" -ne 0 ]; then
      fail "$APP_NAME executable exited $VERSION_RC on --version: $VERSION_OUT"
    else
      case "$VERSION_OUT" in
        *"$EXPECT_VERSION"*)
          ok "--version reports the expected string ('$EXPECT_VERSION'): $VERSION_OUT" ;;
        *)
          fail "--version did not report the expected string ('$EXPECT_VERSION'), got: $VERSION_OUT" ;;
      esac
    fi
  fi
fi

# --- 5. config parses as TOML --------------------------------------------------

if [ -z "$CONFIG_NAME" ]; then
  fail "cannot check config parseability: no single top-level config member resolved above"
elif [ ! -f "$TMP_EXTRACT/$CONFIG_NAME" ]; then
  fail "config member not found on disk after extraction: $CONFIG_NAME"
elif ! command -v python3 >/dev/null 2>&1; then
  info "config TOML-parse check: not verified here (python3 not available)"
else
  TOML_ERR="$(python3 -c "
import sys, tomllib
with open(sys.argv[1], 'rb') as f:
    tomllib.load(f)
" "$TMP_EXTRACT/$CONFIG_NAME" 2>&1)"
  if [ $? -eq 0 ]; then
    ok "$CONFIG_NAME parses as TOML"
  else
    fail "$CONFIG_NAME does not parse as TOML: $(printf '%s' "$TOML_ERR" | tail -1)"
  fi
fi

# --- 6. signing / notarization state — REPORTED, gated only with --require-signed

report_signing() {
  local target="$1"
  if ! command -v codesign >/dev/null 2>&1; then
    info "SIGNING: codesign not available in this environment — not verified here"
    [ -n "$REQUIRE_SIGNED" ] && fail "SIGNING required (--require-signed) but codesign is unavailable to check it"
    return
  fi
  local cs_out cs_rc
  cs_out="$(codesign -dv --verbose=4 "$target" 2>&1)"
  cs_rc=$?
  if [ "$cs_rc" -ne 0 ]; then
    info "SIGNING: $target is NOT signed (codesign exit $cs_rc): $(printf '%s' "$cs_out" | tail -1)"
    if [ -n "$REQUIRE_SIGNED" ]; then
      fail "SIGNING required (--require-signed) but $target is not signed"
    fi
    return
  fi
  if ! command -v spctl >/dev/null 2>&1; then
    info "SIGNING: $target is signed, but spctl is unavailable — notarization not verified here"
    [ -n "$REQUIRE_SIGNED" ] && fail "SIGNING required (--require-signed) but spctl is unavailable to check notarization"
    return
  fi
  local sp_out sp_rc
  sp_out="$(spctl -a -t exec -vv "$target" 2>&1)"
  sp_rc=$?
  if [ "$sp_rc" -eq 0 ]; then
    info "SIGNING: $target is signed and passes Gatekeeper assessment: $sp_out"
    [ -n "$REQUIRE_SIGNED" ] && ok "SIGNING: $target is signed and Gatekeeper-accepted"
  else
    info "SIGNING: $target is signed but FAILS Gatekeeper assessment (not notarized, or notarization not verified here): $sp_out"
    [ -n "$REQUIRE_SIGNED" ] && fail "SIGNING required (--require-signed) but $target fails Gatekeeper assessment: $sp_out"
  fi
}

report_signing "$APP_DIR"

summarize_and_exit
