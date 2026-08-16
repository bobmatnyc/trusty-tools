#!/usr/bin/env bash
#
# verify-trusty-audit-handoff-selftest.sh — failing-case fixtures for
# verify-trusty-audit-handoff.sh (the trusty-audit handoff packaging path,
# #5483's real target layout — see that script's header for what it checks).
#
# Why: a verifier only proves its worth by what it REJECTS — the same
#   principle behind check_public_docs_selftest.sh and
#   `check_adr.sh --self-test`. This asserts a clean baseline PASSES first
#   (mirroring check_adr.sh's "clean corpus passes" assertion, which is what
#   makes each later mutation meaningful — without it, a mutation "failing"
#   could just mean the harness itself is broken), then mutates one fixture
#   at a time and asserts the verifier FAILS with the expected reason on
#   its output — including the #5620 shape (an absent/empty zip must FAIL,
#   never read as "nothing found, pass").
#
#   Fixtures are built with `cc`, not copied from anywhere real: a trivial C
#   program compiled for a chosen architecture, wrapped in a hand-built .app
#   bundle. This is deliberately NOT a real build — unsigned, bundle ID says
#   "test-fixture-NOT-REAL", --version string says "-selftest". Never mistake
#   this script's output for a shippable artifact.
#
# Requires: macOS with Xcode Command Line Tools (cc, lipo, file, plutil,
#   zip, unzip) and python3 — the same platform this tooling targets
#   (#5483 is arm64-only). SKIPs (exit 0) rather than failing if any of
#   these are missing, since that means the self-test cannot exercise the
#   verifier at all, not that the verifier is broken.
#
# Test: this IS the test. Run directly:
#   bash scripts/verify-trusty-audit-handoff-selftest.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGE="$SCRIPT_DIR/package-trusty-audit-handoff.sh"
VERIFY="$SCRIPT_DIR/verify-trusty-audit-handoff.sh"

VERSION_STRING="0.0.0-selftest"

for tool in cc lipo file zip unzip python3 plutil; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "SKIP: '$tool' not available — this self-test requires macOS + Xcode Command Line Tools" >&2
    exit 0
  fi
done

passes=0
failures=0

# --- fixture builders ---------------------------------------------------------

# build_app <dir> <arch> -> prints the path to a synthetic .app bundle whose
# single executable is compiled for <arch> ("arm64" or "x86_64").
build_app() {
  local dir="$1" arch="$2"
  local app="$dir/TrustyAuditFixture.app"
  mkdir -p "$app/Contents/MacOS"
  cat >"$dir/main.c" <<'CSRC'
#include <stdio.h>
#include <string.h>
int main(int argc, char **argv) {
  if (argc > 1 && strcmp(argv[1], "--version") == 0) {
    printf("trusty-audit-fixture 0.0.0-selftest\n");
    return 0;
  }
  return 1;
}
CSRC
  cc -arch "$arch" -o "$app/Contents/MacOS/TrustyAuditFixture" "$dir/main.c" 2>&1
  cat >"$app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key>
  <string>TrustyAuditFixture</string>
  <key>CFBundleIdentifier</key>
  <string>com.trusty-mpm.test-fixture.NOT-REAL</string>
  <key>CFBundleName</key>
  <string>TrustyAuditFixture (synthetic self-test fixture, not a real build)</string>
</dict>
</plist>
PLIST
  printf '%s' "$app"
}

build_config() {
  local dir="$1"
  local cfg="$dir/engagement.toml"
  cat >"$cfg" <<'TOML'
# Synthetic self-test fixture — NOT a real engagement config (#5478 does not
# exist in this repo yet). Shape is illustrative only.
engagement_name = "selftest-fixture"
audit_window_weeks = 52
TOML
  printf '%s' "$cfg"
}

build_readme() {
  local dir="$1"
  local rd="$dir/SOURCE-README.md"
  printf '# trusty-audit handoff (self-test fixture)\n\nSynthetic, not a real deliverable.\n' >"$rd"
  printf '%s' "$rd"
}

# expect <label> <expected_exit> <expected_substring|-> -- <cmd...>
expect() {
  local label="$1" expected_exit="$2" expected_sub="$3"
  shift 3
  local out rc
  out="$("$@" 2>&1)"
  rc=$?
  if [ "$rc" -ne "$expected_exit" ]; then
    echo "FAIL: $label -> exit $rc (expected $expected_exit)" >&2
    printf '%s\n' "$out" | sed 's/^/       /' >&2
    failures=$((failures + 1))
    return
  fi
  if [ "$expected_sub" != "-" ] && ! printf '%s\n' "$out" | grep -qF -- "$expected_sub"; then
    echo "FAIL: $label -> exit $rc as expected, but output never mentions '$expected_sub'" >&2
    printf '%s\n' "$out" | sed 's/^/       /' >&2
    failures=$((failures + 1))
    return
  fi
  echo "  ok   $label -> exit $rc"
  printf '%s\n' "$out" | sed 's/^/       /'
  passes=$((passes + 1))
}

TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

# --- baseline: a clean synthetic artifact must PASS ---------------------------
# This is the assertion that makes every case below meaningful: if the
# harness itself were broken (wrong fixture shape, wrong package-script
# invocation), a mutation "failing" downstream would prove nothing.

clean_dir="$TMP_ROOT/clean"
mkdir -p "$clean_dir"
app_path="$(build_app "$clean_dir" arm64)"
cfg_path="$(build_config "$clean_dir")"
rd_path="$(build_readme "$clean_dir")"
good_zip="$TMP_ROOT/good.zip"

if ! pkg_out="$(bash "$PACKAGE" --app "$app_path" --config "$cfg_path" --readme "$rd_path" --out "$good_zip" 2>&1)"; then
  echo "FAIL: package script could not build the baseline fixture" >&2
  printf '%s\n' "$pkg_out" | sed 's/^/       /' >&2
  failures=$((failures + 1))
else
  expect "clean synthetic artifact passes" 0 "VERIFY: PASS" \
    bash "$VERIFY" --zip "$good_zip" --expect-version "$VERSION_STRING"
fi

# --- case: a member missing --------------------------------------------------

missing_zip="$TMP_ROOT/missing-member.zip"
cp "$good_zip" "$missing_zip"
zip -d "$missing_zip" "README.md" >/dev/null
expect "member missing (README.md)" 1 "missing top-level README.md" \
  bash "$VERIFY" --zip "$missing_zip" --expect-version "$VERSION_STRING"

# --- case: wrong architecture (x86_64 where arm64 is required) ---------------

wrong_arch_dir="$TMP_ROOT/wrong-arch"
mkdir -p "$wrong_arch_dir"
wa_app="$(build_app "$wrong_arch_dir" x86_64)"
wa_cfg="$(build_config "$wrong_arch_dir")"
wa_rd="$(build_readme "$wrong_arch_dir")"
wrong_arch_zip="$TMP_ROOT/wrong-arch.zip"
bash "$PACKAGE" --app "$wa_app" --config "$wa_cfg" --readme "$wa_rd" --out "$wrong_arch_zip" >/dev/null
expect "wrong architecture (x86_64)" 1 "NOT arm64" \
  bash "$VERIFY" --zip "$wrong_arch_zip" --expect-version "$VERSION_STRING"

# --- case: truncated / corrupt .app executable --------------------------------
# Truncated to 4 bytes: enough for `file` to still see the Mach-O magic, not
# enough to see the cputype field — so it reports "Mach-O 64-bit" with no
# architecture, which is what a genuinely truncated download looks like.

corrupt_dir="$TMP_ROOT/corrupt"
mkdir -p "$corrupt_dir"
c_app="$(build_app "$corrupt_dir" arm64)"
dd if="$c_app/Contents/MacOS/TrustyAuditFixture" of="$c_app/Contents/MacOS/TrustyAuditFixture.trunc" bs=1 count=4 2>/dev/null
mv "$c_app/Contents/MacOS/TrustyAuditFixture.trunc" "$c_app/Contents/MacOS/TrustyAuditFixture"
c_cfg="$(build_config "$corrupt_dir")"
c_rd="$(build_readme "$corrupt_dir")"
corrupt_zip="$TMP_ROOT/corrupt.zip"
bash "$PACKAGE" --app "$c_app" --config "$c_cfg" --readme "$c_rd" --out "$corrupt_zip" >/dev/null
expect "truncated/corrupt .app executable" 1 "corrupt or truncated" \
  bash "$VERIFY" --zip "$corrupt_zip" --expect-version "$VERSION_STRING"

# --- case: unparseable config --------------------------------------------------

badcfg_dir="$TMP_ROOT/badcfg"
mkdir -p "$badcfg_dir"
bc_app="$(build_app "$badcfg_dir" arm64)"
bc_cfg="$badcfg_dir/engagement.toml"
printf 'this is not [valid toml\n' >"$bc_cfg"
bc_rd="$(build_readme "$badcfg_dir")"
badcfg_zip="$TMP_ROOT/badcfg.zip"
bash "$PACKAGE" --app "$bc_app" --config "$bc_cfg" --readme "$bc_rd" --out "$badcfg_zip" >/dev/null
expect "unparseable config" 1 "does not parse as TOML" \
  bash "$VERIFY" --zip "$badcfg_zip" --expect-version "$VERSION_STRING"

# --- case: empty / absent zip (#5620 shape — must FAIL, never read as pass) --

absent_zip="$TMP_ROOT/does-not-exist.zip"
expect "absent zip (#5620 shape)" 1 "does not exist" \
  bash "$VERIFY" --zip "$absent_zip" --expect-version "$VERSION_STRING"

empty_zip="$TMP_ROOT/empty.zip"
: >"$empty_zip"
expect "empty zip (#5620 shape)" 1 "empty" \
  bash "$VERIFY" --zip "$empty_zip" --expect-version "$VERSION_STRING"

echo ""
if [ "$failures" -gt 0 ]; then
  printf 'verify-trusty-audit-handoff self-test: %d/%d case(s) failed\n' \
    "$failures" "$((passes + failures))" >&2
  exit 1
fi
printf 'verify-trusty-audit-handoff self-test: all %d cases passed\n' "$passes"
