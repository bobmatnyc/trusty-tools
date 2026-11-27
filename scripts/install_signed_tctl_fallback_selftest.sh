#!/usr/bin/env bash
#
# install_signed_tctl_fallback_selftest.sh — self-test for the signer
# resolution in scripts/install-trusty-search-signed.sh and
# scripts/install-trusty-memory-signed.sh (#11683).
#
# Why: both scripts sign through `tctl sign <set>`. A tctl installed before
#   its set existed fails that call with a clap usage error, and the
#   `cargo run -p trusty-installer` fallback without `--bin tctl` fails with
#   "could not determine which binary to run", because the crate has two
#   binaries. Either way nothing is signed, and the memory script only warned.
# What: runs each script with a stub `cargo` (install writes the binaries into
#   a temp CARGO_HOME; `run` without `--bin tctl` fails the way real cargo
#   does) and a stub `tctl` (its `sign -h` lists the set under <TARGET>'s
#   `[possible values: ...]` unless STUB_TCTL_OLD=1, which names the set only
#   in prose). PATH holds only the stubs and /usr/bin:/bin, so no real
#   cargo, tctl or codesign runs. Cases, per script:
#     current-tctl        a tctl that accepts the set signs; cargo never runs
#                         the fallback
#     stale-tctl          a tctl that does not accept the set (but names it
#                         outside the possible-values list) is named stale
#                         and never asked to sign; the fallback signs with
#                         `--bin tctl`
#     no-tctl             no tctl on PATH; the fallback signs with `--bin tctl`
#   and per script:
#     search sign-fails   a failing sign exits 1 with an ERROR line
#     memory sign-soft    a sign exiting 3 (no certificate) warns, exits 0
#     memory sign-fails   a sign exiting 1 (any other failure) prints an
#                         ERROR line and exits 1 (#11683)
#     memory unbuildable  a fallback tctl that will not build exits 1
#
# Usage: ./scripts/install_signed_tctl_fallback_selftest.sh
# Exit:  0 when every case behaves (or skipped off macOS); 1 naming each
#        failing case.
# Portability: bash 3.2 (macOS). The scripts under test are macOS-only.

# #7812: re-exec under bash when started from zsh.
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

if [ "$(uname -s)" != "Darwin" ]; then
  echo "install_signed_tctl_fallback_selftest: skipped (the scripts are macOS-only)"
  exit 0
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
WORK="$(mktemp -d /tmp/tsignfb.XXXXXX)"
WORK="$(cd "$WORK" && pwd -P)"
trap 'rm -rf "$WORK"' EXIT

FAILED=""
fail() {
  echo "FAIL: $1" >&2
  FAILED="$FAILED $1"
}

# Two stub dirs: cargo alone, and cargo plus tctl.
NO_TCTL="$WORK/stubs-no-tctl"
WITH_TCTL="$WORK/stubs-tctl"
mkdir -p "$NO_TCTL" "$WITH_TCTL"
cat > "$NO_TCTL/cargo" <<'STUB'
#!/usr/bin/env bash
set -eu
printf '%s\n' "$*" >> "$STUB_LOG/cargo.argv"
case "${1:-}" in
  install)
    mkdir -p "$CARGO_HOME/bin"
    for b in trusty-search trusty-embedderd trusty-embedderd-py trusty-memory trusty-memory-mcp-bridge; do
      printf 'bin\n' > "$CARGO_HOME/bin/$b"
    done
    exit 0 ;;
  build)
    if [ "${STUB_CARGO_BUILD_FAIL:-0}" = "1" ]; then exit 101; fi
    exit 0 ;;
  run)
    case " $* " in
      *" --bin tctl "*) ;;
      *) echo "error: \`cargo run\` could not determine which binary to run" >&2; exit 101 ;;
    esac
    exit "${STUB_SIGN_RC:-0}" ;;
  *) exit 97 ;;
esac
STUB
cp "$NO_TCTL/cargo" "$WITH_TCTL/cargo"
cat > "$WITH_TCTL/tctl" <<'STUB'
#!/usr/bin/env bash
set -eu
if [ "${2:-}" = "-h" ] || [ "${2:-}" = "--help" ]; then
  # STUB_TCTL_OLD: a tctl that predates every set this test signs; its prose
  # still names them, so only the possible-values list may decide (#11683).
  if [ "${STUB_TCTL_OLD:-0}" = "1" ]; then
    echo "Developer-ID codesign; scripts/install-trusty-search-signed.sh and trusty-memory call this"
    echo "  <TARGET>  Which signable set to sign [possible values: trusty-mpm, trusty-agents]"
  else
    echo "  <TARGET>  Which signable set to sign [possible values: trusty-search, trusty-mpm, trusty-memory, trusty-assistants]"
  fi
  echo "      --scope <SCOPE>  Scope to act on [possible values: project, system, all]"
  exit 0
fi
printf '%s\n' "$*" >> "$STUB_LOG/tctl.argv"
exit "${STUB_SIGN_RC:-0}"
STUB
chmod +x "$NO_TCTL/cargo" "$WITH_TCTL/cargo" "$WITH_TCTL/tctl"

# run SCRIPT CASE STUBDIR: run SCRIPT with STUBDIR first on PATH; sets $RC and
# leaves its output in $WORK/CASE.out and the stub logs in $WORK/CASE/.
run() {
  local script="$1" name="$2" stubs="$3"
  mkdir -p "$WORK/$name"
  RC=0
  STUB_LOG="$WORK/$name" PATH="$stubs:/usr/bin:/bin" CARGO_HOME="$WORK/$name/cargo-home" \
    "$SCRIPT_DIR/$script" > "$WORK/$name.out" 2>&1 || RC=$?
}

# fallback_signed CASE SET: the fallback ran `tctl sign SET` via `--bin tctl`.
fallback_signed() {
  grep -q "^run --quiet -p trusty-installer --bin tctl -- sign $2 --dir $WORK/$1/cargo-home/bin\$" \
    "$WORK/$1/cargo.argv" 2>/dev/null
}

for pair in search:install-trusty-search-signed.sh memory:install-trusty-memory-signed.sh; do
  short="${pair%%:*}"
  script="${pair#*:}"
  set_name="trusty-$short"

  # --- current-tctl -----------------------------------------------------
  run "$script" "$short-current" "$WITH_TCTL"
  [ "$RC" -eq 0 ] || fail "$short current-tctl: exit $RC: $(cat "$WORK/$short-current.out")"
  grep -q "^sign $set_name --dir " "$WORK/$short-current/tctl.argv" 2>/dev/null \
    || fail "$short current-tctl: tctl was not asked to sign $set_name"
  ! grep -q '^run ' "$WORK/$short-current/cargo.argv" || fail "$short current-tctl: fallback ran"

  # --- stale-tctl -------------------------------------------------------
  STUB_TCTL_OLD=1 run "$script" "$short-stale" "$WITH_TCTL"
  [ "$RC" -eq 0 ] || fail "$short stale-tctl: exit $RC: $(cat "$WORK/$short-stale.out")"
  [ ! -e "$WORK/$short-stale/tctl.argv" ] || fail "$short stale-tctl: the stale tctl was asked to sign"
  grep -q "tctl on PATH is stale" "$WORK/$short-stale.out" || fail "$short stale-tctl: staleness not reported"
  fallback_signed "$short-stale" "$set_name" || fail "$short stale-tctl: no '--bin tctl' fallback sign"

  # --- no-tctl ----------------------------------------------------------
  run "$script" "$short-none" "$NO_TCTL"
  [ "$RC" -eq 0 ] || fail "$short no-tctl: exit $RC: $(cat "$WORK/$short-none.out")"
  fallback_signed "$short-none" "$set_name" || fail "$short no-tctl: no '--bin tctl' fallback sign"
done

# --- search sign-fails ----------------------------------------------------
STUB_SIGN_RC=1 run install-trusty-search-signed.sh search-signfail "$WITH_TCTL"
[ "$RC" -eq 1 ] || fail "search sign-fails: exit $RC, want 1"
grep -q 'ERROR: Signing trusty-search failed' "$WORK/search-signfail.out" || fail "search sign-fails: no ERROR line"

# --- memory sign-soft (#11683: exit 3 = no certificate) --------------------
STUB_SIGN_RC=3 run install-trusty-memory-signed.sh memory-signsoft "$WITH_TCTL"
[ "$RC" -eq 0 ] || fail "memory sign-soft: exit $RC, want 0"
grep -q 'WARNING: Signing the trusty-memory set failed (exit 3: no Developer ID certificate)' \
  "$WORK/memory-signsoft.out" || fail "memory sign-soft: no warning"

# --- memory sign-fails (#11683: any other sign exit) -----------------------
STUB_SIGN_RC=1 run install-trusty-memory-signed.sh memory-signfail "$WITH_TCTL"
[ "$RC" -eq 1 ] || fail "memory sign-fails: exit $RC, want 1"
grep -q 'ERROR: Signing the trusty-memory set failed (exit 1)' "$WORK/memory-signfail.out" \
  || fail "memory sign-fails: no ERROR line"

# --- memory unbuildable ---------------------------------------------------
STUB_CARGO_BUILD_FAIL=1 run install-trusty-memory-signed.sh memory-unbuildable "$NO_TCTL"
[ "$RC" -eq 1 ] || fail "memory unbuildable: exit $RC, want 1"
grep -q 'could not build this checkout' "$WORK/memory-unbuildable.out" || fail "memory unbuildable: no error"
! grep -q '^run ' "$WORK/memory-unbuildable/cargo.argv" || fail "memory unbuildable: sign ran anyway"

if [ -n "$FAILED" ]; then
  echo "install_signed_tctl_fallback_selftest: FAILED:$FAILED" >&2
  exit 1
fi
echo "install_signed_tctl_fallback_selftest: all cases passed"
