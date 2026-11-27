#!/usr/bin/env bash
#
# install_trusty_assistants_signed_selftest.sh — self-test for
# scripts/install-trusty-assistants-signed.sh (#11650 slice 3d).
#
# Why: the script's job is what it does NOT touch: the `tagent` a launchd
#   Slack bot runs. An install that replaced or re-signed that file would
#   still print success, so only a check of the file itself proves the guard.
# What: runs the script against temp `--root` directories with a stub `cargo`
#   (writes `<root>/bin/ta`, records argv) and a stub `tctl` (appends a
#   "signed" line to EVERY file in its `--dir`, the way a `tctl` whose
#   assistants set still lists `tagent` would). No real build, install or
#   codesign runs. Cases:
#     tagent-untouched  a pre-existing `<root>/bin/tagent` keeps its inode,
#                       size, mtime and sha256; `ta` is installed and signed;
#                       cargo got `--bin ta` and never `tagent`; tctl saw a
#                       directory holding only `ta`; no staging dir is left
#     tagent-absent     a root with no `tagent` still has none afterwards
#     guard-trips       a cargo that rewrites `tagent` makes the script exit 1
#     refuse-bin        `--bin tagent`, `--bin ta`, `--bin=tagent` and a bare
#                       `tagent` exit 2 before cargo runs
#     fingerprint-fails a failing or empty-output `shasum` or `stat` makes the
#                       script exit 1 before cargo runs, never "unchanged"
#     path-cargo-home   `--from-source` into the cargo home exits 2 (ADR-0043)
#     sign-fail-soft    a tctl exiting 3 (no certificate) leaves `ta` in
#                       place and exits 0 (#11683)
#     sign-fail-hard    a tctl exiting 1 (any other sign failure) leaves `ta`
#                       in place and exits 1 with an ERROR line (#11683)
#     old-tctl          a tctl that does not accept `trusty-assistants` is
#                       never asked to sign; this checkout's trusty-installer
#                       is built and signs the staging dir (only `ta`)
#     signer-unbuildable  that fallback failing to build exits 1 (#11683)
#     source-argv      `--git-rev` and the registry default build the
#                       expected `cargo install` argv
#     fresh-target      cargo install gets CARGO_NET_GIT_FETCH_WITH_CLI=true,
#                       no inherited CARGO_TARGET_DIR, and a `--target-dir`
#                       that is new, empty and removed afterwards (#11733)
#     target-cleanup    a failing cargo (exit code kept) and a TERM sent to the
#                       script mid-install both leave no --target-dir behind
#                       and a non-zero exit (#11733)
#     dry-run           runs neither stub and creates nothing; the printed
#                       command shows git-CLI fetch and the fresh target dir
#
# Usage: ./scripts/install_trusty_assistants_signed_selftest.sh
# Exit:  0 when every case behaves (or skipped off macOS); 1 naming each
#        failing case.
# Portability: bash 3.2 (macOS). The script under test is macOS-only.

# #7812: re-exec under bash when started from zsh.
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

if [ "$(uname -s)" != "Darwin" ]; then
  echo "install_trusty_assistants_signed_selftest: skipped (the script is macOS-only)"
  exit 0
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
SUT="$SCRIPT_DIR/install-trusty-assistants-signed.sh"
WORK="$(mktemp -d /tmp/tasig.XXXXXX)"
WORK="$(cd "$WORK" && pwd -P)"
trap 'rm -rf "$WORK"' EXIT

FAILED=""
fail() {
  echo "FAIL: $1" >&2
  FAILED="$FAILED $1"
}

STUBS="$WORK/stubs"
mkdir -p "$STUBS"
cat > "$STUBS/cargo" <<'STUB'
#!/usr/bin/env bash
set -eu
printf '%s\n' "$*" >> "$STUB_LOG/cargo.argv"
case "${1:-}" in
  install) ;;
  build)
    # #11683: STUB_CARGO_BUILD_FAIL=1 makes the fallback signer unbuildable.
    if [ "${STUB_CARGO_BUILD_FAIL:-0}" = "1" ]; then exit 101; fi
    exit 0 ;;
  run)
    # `cargo run -p trusty-installer -- sign <set> --dir D`: sign like tctl.
    dir=""
    while [ "$#" -gt 0 ]; do
      if [ "$1" = "--dir" ]; then dir="$2"; fi
      shift
    done
    ls -A "$dir" > "$STUB_LOG/tctl.dir"
    for f in "$dir"/*; do printf 'signed\n' >> "$f"; done
    exit 0 ;;
  *) exit 97 ;;
esac
root=""
target=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--root" ]; then root="$2"; fi
  if [ "$1" = "--target-dir" ]; then target="$2"; fi
  shift
done
# #11733: the env and target dir this install was given.
{
  printf 'CARGO_NET_GIT_FETCH_WITH_CLI=%s\n' "${CARGO_NET_GIT_FETCH_WITH_CLI:-}"
  printf 'CARGO_TARGET_DIR=%s\n' "${CARGO_TARGET_DIR:-}"
  printf 'target-dir=%s\n' "$target"
  if [ -n "$target" ] && [ -d "$target" ] && [ -z "$(ls -A "$target")" ]; then
    echo "target-dir-state=empty"
  else
    echo "target-dir-state=missing-or-used"
  fi
} > "$STUB_LOG/cargo.env"
# #11733: STUB_CARGO_FAIL=<code> fails the install; STUB_CARGO_TERM=1 sends
# TERM to the script (our parent) as a Ctrl-C / kill would, then exits 0.
if [ -n "${STUB_CARGO_FAIL:-}" ]; then exit "$STUB_CARGO_FAIL"; fi
if [ "${STUB_CARGO_TERM:-0}" = "1" ]; then kill -TERM "$PPID"; exit 0; fi
mkdir -p "$root/bin"
printf 'ta-binary\n' > "$root/bin/ta"
chmod +x "$root/bin/ta"
if [ "${STUB_CARGO_TOUCH_TAGENT:-0}" = "1" ]; then
  printf 'replaced\n' > "$root/bin/tagent"
fi
STUB
cat > "$STUBS/tctl" <<'STUB'
#!/usr/bin/env bash
set -eu
if [ "${2:-}" = "--help" ]; then
  # STUB_TCTL_OLD: a tctl from before #11632, which knows only trusty-agents.
  if [ "${STUB_TCTL_OLD:-0}" = "1" ]; then
    echo "[possible values: trusty-search, trusty-mpm, trusty-agents]"
  else
    echo "[possible values: trusty-search, trusty-mpm, trusty-assistants]"
  fi
  exit 0
fi
printf '%s\n' "$*" >> "$STUB_LOG/tctl.argv"
dir=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--dir" ]; then dir="$2"; fi
  shift
done
ls -A "$dir" > "$STUB_LOG/tctl.dir"
# #11683: STUB_TCTL_FAIL=<code> makes the sign step exit <code>.
if [ "${STUB_TCTL_FAIL:-0}" != "0" ]; then exit "$STUB_TCTL_FAIL"; fi
for f in "$dir"/*; do printf 'signed\n' >> "$f"; done
STUB
chmod +x "$STUBS/cargo" "$STUBS/tctl"

# run CASE ARGS...: run the script with the stubs first on PATH; sets $RC,
# and leaves stdout+stderr in $WORK/CASE.out and the stub logs in $WORK/CASE/.
run() {
  local name="$1"
  shift
  mkdir -p "$WORK/$name"
  RC=0
  STUB_LOG="$WORK/$name" PATH="${STUB_EXTRA_PATH:+$STUB_EXTRA_PATH:}$STUBS:/usr/bin:/bin" CARGO_HOME="$WORK/cargo-home" \
    "$SUT" "$@" > "$WORK/$name.out" 2>&1 || RC=$?
}

fp() {
  if [ ! -e "$1" ]; then echo absent; return; fi
  echo "$(stat -f '%i %z %m' "$1") $(shasum -a 256 "$1" | cut -d' ' -f1)"
}

new_root_with_tagent() {
  local root="$WORK/$1/root"
  mkdir -p "$root/bin"
  printf 'live tagent the launchd bot runs\n' > "$root/bin/tagent"
  chmod +x "$root/bin/tagent"
  touch -t 202610281508 "$root/bin/tagent"
  echo "$root"
}

# --- tagent-untouched ---------------------------------------------------
root="$(new_root_with_tagent tagent-untouched)"
before="$(fp "$root/bin/tagent")"
run tagent-untouched --from-source --root "$root"
after="$(fp "$root/bin/tagent")"
[ "$RC" -eq 0 ] || fail "tagent-untouched: exit $RC: $(cat "$WORK/tagent-untouched.out")"
[ "$before" = "$after" ] || fail "tagent-untouched: tagent changed [$before] -> [$after]"
grep -q 'signed' "$root/bin/ta" 2>/dev/null || fail "tagent-untouched: ta was not signed in place"
[ "$(cat "$WORK/tagent-untouched/tctl.dir" 2>/dev/null)" = "ta" ] \
  || fail "tagent-untouched: tctl saw [$(cat "$WORK/tagent-untouched/tctl.dir" 2>/dev/null)], not only ta"
argv="$(cat "$WORK/tagent-untouched/cargo.argv")"
case "$argv" in *"--bin ta --locked --root $root --target-dir "*) ;; *) fail "tagent-untouched: cargo argv [$argv]" ;; esac
for word in $argv; do
  [ "$word" != "tagent" ] || fail "tagent-untouched: cargo argv names tagent [$argv]"
done
case "$argv" in *"--path $SCRIPT_DIR/../crates/trusty-assistants"*|*"--path "*"/crates/trusty-assistants"*) ;; *) fail "tagent-untouched: not a path install [$argv]" ;; esac
leftover="$(find "$root" -maxdepth 1 -name '.ta-sign.*' | head -n 1)"
[ -z "$leftover" ] || fail "tagent-untouched: staging dir left behind: $leftover"

# --- tagent-absent ------------------------------------------------------
root="$WORK/tagent-absent/root"
mkdir -p "$root"
run tagent-absent --from-source --root "$root"
[ "$RC" -eq 0 ] || fail "tagent-absent: exit $RC"
[ ! -e "$root/bin/tagent" ] || fail "tagent-absent: a tagent appeared"
[ -f "$root/bin/ta" ] || fail "tagent-absent: ta not installed"

# --- guard-trips --------------------------------------------------------
root="$(new_root_with_tagent guard-trips)"
STUB_CARGO_TOUCH_TAGENT=1 run guard-trips --from-source --root "$root"
[ "$RC" -eq 1 ] || fail "guard-trips: exit $RC, want 1"
grep -q 'changed during the install' "$WORK/guard-trips.out" || fail "guard-trips: no 'changed' error"

# --- fingerprint-fails (#11650) ------------------------------------------
# A hash or stat tool that fails, or prints nothing, must stop the script:
# otherwise before == after trivially and a changed tagent goes unnoticed.
for tool in shasum stat; do
  for mode in fail empty; do
    name="fingerprint-$tool-$mode"
    mkdir -p "$WORK/$name-bin"
    if [ "$mode" = "fail" ]; then
      printf '#!/bin/sh\nexit 1\n' > "$WORK/$name-bin/$tool"
    else
      printf '#!/bin/sh\nexit 0\n' > "$WORK/$name-bin/$tool"
    fi
    chmod +x "$WORK/$name-bin/$tool"
    root="$(new_root_with_tagent "$name")"
    STUB_EXTRA_PATH="$WORK/$name-bin" run "$name" --from-source --root "$root"
    [ "$RC" -eq 1 ] || fail "$name: exit $RC, want 1"
    grep -q 'cannot fingerprint' "$WORK/$name.out" || fail "$name: no 'cannot fingerprint' error"
    ! grep -q 'unchanged' "$WORK/$name.out" || fail "$name: reported unchanged"
    [ ! -e "$WORK/$name/cargo.argv" ] || fail "$name: cargo ran"
  done
done

# --- refuse-bin ---------------------------------------------------------
i=0
for args in "--bin tagent" "--bin ta" "--bin=tagent" "tagent"; do
  i=$((i + 1))
  root="$(new_root_with_tagent "refuse-bin-$i")"
  # shellcheck disable=SC2086  # word-split on purpose
  run "refuse-bin-$i" --from-source --root "$root" $args
  [ "$RC" -eq 2 ] || fail "refuse-bin [$args]: exit $RC, want 2"
  [ ! -e "$WORK/refuse-bin-$i/cargo.argv" ] || fail "refuse-bin [$args]: cargo ran"
  grep -q 'installs only .ta.' "$WORK/refuse-bin-$i.out" || fail "refuse-bin [$args]: no refusal text"
done

# --- path-cargo-home ----------------------------------------------------
mkdir -p "$WORK/cargo-home"
run path-cargo-home --from-source
[ "$RC" -eq 2 ] || fail "path-cargo-home: exit $RC, want 2"
grep -q 'ADR-0043' "$WORK/path-cargo-home.out" || fail "path-cargo-home: no ADR-0043 refusal"
[ ! -e "$WORK/path-cargo-home/cargo.argv" ] || fail "path-cargo-home: cargo ran"

# --- sign-fail-soft -----------------------------------------------------
root="$(new_root_with_tagent sign-fail-soft)"
before="$(fp "$root/bin/tagent")"
STUB_TCTL_FAIL=3 run sign-fail-soft --from-source --root "$root"
[ "$RC" -eq 0 ] || fail "sign-fail-soft: exit $RC, want 0"
[ "$(cat "$root/bin/ta" 2>/dev/null)" = "ta-binary" ] || fail "sign-fail-soft: ta not restored unsigned"
[ "$(fp "$root/bin/tagent")" = "$before" ] || fail "sign-fail-soft: tagent changed"
grep -q 'WARNING: signing ta failed (exit 3: no Developer ID certificate)' "$WORK/sign-fail-soft.out" \
  || fail "sign-fail-soft: no warning"

# --- sign-fail-hard (#11683) -----------------------------------------------
root="$(new_root_with_tagent sign-fail-hard)"
before="$(fp "$root/bin/tagent")"
STUB_TCTL_FAIL=1 run sign-fail-hard --from-source --root "$root"
[ "$RC" -eq 1 ] || fail "sign-fail-hard: exit $RC, want 1"
[ "$(cat "$root/bin/ta" 2>/dev/null)" = "ta-binary" ] || fail "sign-fail-hard: ta not restored unsigned"
[ "$(fp "$root/bin/tagent")" = "$before" ] || fail "sign-fail-hard: tagent changed"
grep -q 'ERROR: signing ta failed (exit 1)' "$WORK/sign-fail-hard.out" || fail "sign-fail-hard: no ERROR line"

# --- old-tctl -----------------------------------------------------------
root="$(new_root_with_tagent old-tctl)"
before="$(fp "$root/bin/tagent")"
STUB_TCTL_OLD=1 run old-tctl --from-source --root "$root"
[ "$RC" -eq 0 ] || fail "old-tctl: exit $RC"
[ ! -e "$WORK/old-tctl/tctl.argv" ] || fail "old-tctl: the pre-#11632 tctl was asked to sign"
grep -q '^build --quiet -p trusty-installer --bin tctl$' "$WORK/old-tctl/cargo.argv" || fail "old-tctl: trusty-installer not built"
grep -q '^run --quiet -p trusty-installer --bin tctl -- sign trusty-assistants --dir .*/\.ta-sign\.' "$WORK/old-tctl/cargo.argv" \
  || fail "old-tctl: no fallback sign on the staging dir"
[ "$(cat "$WORK/old-tctl/tctl.dir" 2>/dev/null)" = "ta" ] || fail "old-tctl: fallback saw more than ta"
grep -q 'signed' "$root/bin/ta" || fail "old-tctl: ta not signed"
[ "$(fp "$root/bin/tagent")" = "$before" ] || fail "old-tctl: tagent changed"

# --- signer-unbuildable (#11683) -----------------------------------------
# A stale tctl plus a checkout whose tctl will not build is a broken install,
# not the no-certificate case: it must exit 1, never warn and exit 0.
root="$(new_root_with_tagent signer-unbuildable)"
before="$(fp "$root/bin/tagent")"
STUB_TCTL_OLD=1 STUB_CARGO_BUILD_FAIL=1 run signer-unbuildable --from-source --root "$root"
[ "$RC" -eq 1 ] || fail "signer-unbuildable: exit $RC, want 1"
grep -q 'could not build this checkout' "$WORK/signer-unbuildable.out" || fail "signer-unbuildable: no error"
[ "$(cat "$root/bin/ta" 2>/dev/null)" = "ta-binary" ] || fail "signer-unbuildable: ta moved or changed"
[ "$(fp "$root/bin/tagent")" = "$before" ] || fail "signer-unbuildable: tagent changed"

# --- source-argv --------------------------------------------------------
root="$WORK/git/root"
mkdir -p "$root"
run git --git-rev 0123abcd --root "$root"
case "$(cat "$WORK/git/cargo.argv")" in
  "install --git https://github.com/bobmatnyc/trusty-tools --rev 0123abcd trusty-assistants --bin ta --locked --root $root --target-dir "?*) ;;
  *) fail "source-argv git: [$(cat "$WORK/git/cargo.argv")]" ;;
esac
root="$WORK/registry/root"
mkdir -p "$root"
run registry --root "$root"
want_version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$SCRIPT_DIR/../crates/trusty-assistants/Cargo.toml" | head -n 1)"
case "$(cat "$WORK/registry/cargo.argv")" in
  "install trusty-assistants --version $want_version --bin ta --locked --root $root --target-dir "?*) ;;
  *) fail "source-argv registry: [$(cat "$WORK/registry/cargo.argv")]" ;;
esac
run bad-rev --git-rev 'main;rm' --root "$root"
[ "$RC" -eq 2 ] || fail "source-argv: a non-hex --git-rev exit $RC, want 2"

# --- fresh-target (#11733) ----------------------------------------------
# Git-CLI fetch authenticates where libgit2 cannot; a fresh target dir keeps
# a previous rev's build-script output (its commit stamp) out of the build.
root="$WORK/fresh/root"
mkdir -p "$root" "$WORK/parent-target"
CARGO_TARGET_DIR="$WORK/parent-target" run fresh --git-rev 0123abcd --root "$root"
[ "$RC" -eq 0 ] || fail "fresh-target: exit $RC: $(cat "$WORK/fresh.out")"
env_log="$(cat "$WORK/fresh/cargo.env" 2>/dev/null || true)"
printf '%s\n' "$env_log" | grep -qx 'CARGO_NET_GIT_FETCH_WITH_CLI=true' \
  || fail "fresh-target: git-CLI fetch not set [$env_log]"
printf '%s\n' "$env_log" | grep -qx 'CARGO_TARGET_DIR=' \
  || fail "fresh-target: cargo inherited CARGO_TARGET_DIR [$env_log]"
target="$(printf '%s\n' "$env_log" | sed -n 's/^target-dir=//p')"
[ -n "$target" ] && [ "$target" != "$WORK/parent-target" ] \
  || fail "fresh-target: --target-dir [$target] is missing or the parent's"
printf '%s\n' "$env_log" | grep -qx 'target-dir-state=empty' \
  || fail "fresh-target: --target-dir was not a new empty dir [$env_log]"
[ -z "$target" ] || [ ! -e "$target" ] || fail "fresh-target: $target left behind"

# --- target-cleanup (#11733) ---------------------------------------------
# The scratch --target-dir must go on a failed install and on an interrupt.
root="$WORK/tc-fail/root"
mkdir -p "$root"
STUB_CARGO_FAIL=7 run tc-fail --git-rev 0123abcd --root "$root"
[ "$RC" -eq 7 ] || fail "target-cleanup fail: exit $RC, want cargo's 7"
target="$(sed -n 's/^target-dir=//p' "$WORK/tc-fail/cargo.env")"
[ -n "$target" ] && [ ! -e "$target" ] || fail "target-cleanup fail: [$target] left behind"
[ ! -e "$WORK/tc-fail/tctl.dir" ] || fail "target-cleanup fail: signing ran after a failed install"
root="$WORK/tc-term/root"
mkdir -p "$root"
STUB_CARGO_TERM=1 run tc-term --git-rev 0123abcd --root "$root"
[ "$RC" -ne 0 ] || fail "target-cleanup term: exit 0 after TERM"
target="$(sed -n 's/^target-dir=//p' "$WORK/tc-term/cargo.env")"
[ -n "$target" ] && [ ! -e "$target" ] || fail "target-cleanup term: [$target] left behind"
[ ! -e "$WORK/tc-term/tctl.dir" ] || fail "target-cleanup term: signing ran after TERM"

# --- dry-run ------------------------------------------------------------
root="$WORK/dry/never"
run dry --git-rev 0123abcd --root "$root" --dry-run
[ "$RC" -eq 0 ] || fail "dry-run: exit $RC"
[ ! -e "$root" ] || fail "dry-run: created $root"
[ ! -e "$WORK/dry/cargo.argv" ] && [ ! -e "$WORK/dry/tctl.argv" ] || fail "dry-run: a stub ran"
dry_line="$(grep '\[DRY RUN\] .*cargo install --git' "$WORK/dry.out" || true)"
[ -n "$dry_line" ] || fail "dry-run: install command not printed"
case "$dry_line" in *"CARGO_NET_GIT_FETCH_WITH_CLI=true"*) ;; *) fail "dry-run: no git-CLI fetch [$dry_line]" ;; esac
case "$dry_line" in *"-u CARGO_TARGET_DIR"*"--target-dir <"*) ;; *) fail "dry-run: no fresh target dir [$dry_line]" ;; esac

if [ -n "$FAILED" ]; then
  echo "install_trusty_assistants_signed_selftest: FAILED:$FAILED" >&2
  exit 1
fi
echo "install_trusty_assistants_signed_selftest: all cases passed"
