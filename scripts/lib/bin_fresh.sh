#!/usr/bin/env bash
#
# bin_fresh.sh — the sandbox launchers' binary-freshness guard (issue #10755).
#
# Why: a `tm tui` live check ran a stale debug `tm`. Builds land in the build
#   lease's pool slot, so the binary a launcher finds on PATH or under the
#   caller's ambient $CARGO_TARGET_DIR can predate the source under test, and
#   nothing said so.
# What: defines `bin_fresh_check PREFIX BIN ALLOW_STALE SRC_ROOT PACKAGE
#   [EXTRA_PATHSPEC...]`. BIN must be an absolute path; a symlink is followed
#   to its final target, and both are printed with the target's mtime.
#   - Build artifact: the target sits under `.../cargo-target-pool/...`, or
#     below any directory holding `CACHEDIR.TAG` or `.rustc_info.json` (any
#     profile). It is compared with the newest file, tracked or untracked but
#     not ignored, under SRC_ROOT in the sources of PACKAGE. Older: returns 1
#     with a refusal naming both mtimes and that file on stderr, unless
#     ALLOW_STALE is 1, which warns and returns 0.
#   - Sources: every workspace crate in `cargo tree -p PACKAGE -e normal,build`
#     (run offline and locked in SRC_ROOT), each contributing src/, build.rs, Cargo.toml, help.yaml and
#     assets/, plus Cargo.lock and the EXTRA_PATHSPECs. Cargo.lock is
#     workspace-wide, so any dependency bump anywhere makes a binary stale.
#   - Test override: BIN_FRESH_SOURCES_CMD replaces `cargo tree` only when
#     BIN_FRESH_TEST=1 is also set, and is then announced on stderr. Set
#     alone (a leaked export), it is ignored with a warning, so it cannot
#     pass a stale binary as fresh.
#     Only compiled inputs count: a changelog fragment does not trigger a
#     rebuild, so counting it would refuse a binary cargo considers current.
#     A failed or empty derivation, or no source file, returns 1 (freshness
#     unknown); ALLOW_STALE still overrides.
#   - Anything else (e.g. ~/.cargo/bin/tm): an installed binary whose mtime
#     says nothing about the checkout. Prints a notice and returns 0. Under
#     the cargo bin dir it adds the package `cargo install --list` names.
# Test: scripts/sandbox_daemon_selftest.sh, scripts/sandbox_code_daemon_selftest.sh
#   (the bin-fresh-* cases).
# Portability: bash 3.2 (macOS) and bash 5 (Linux); BSD and GNU stat/date.

# The stat option and format that print "<mtime> <name>": BSD -f, GNU -c.
case "$(uname -s)" in
  Darwin|*BSD) BF_STAT_OPT=-f; BF_STAT_FMT='%m %N' ;;
  *) BF_STAT_OPT=-c; BF_STAT_FMT='%Y %n' ;;
esac

# bin_fresh_fmt EPOCH: a local timestamp for a message.
bin_fresh_fmt() {
  case "$(uname -s)" in
    Darwin) date -r "$1" '+%Y-%m-%d %H:%M:%S' 2>/dev/null || echo "$1" ;;
    *) date -d "@$1" '+%Y-%m-%d %H:%M:%S' 2>/dev/null || echo "$1" ;;
  esac
}

# bin_fresh_resolve PATH: the final target of a symlink chain, with a physical
# directory, on stdout. No `readlink -f` (absent on older macOS). Fails on a
# loop (over 40 hops) or an unresolvable directory.
bin_fresh_resolve() {
  local p="$1" t d n=0
  while [ -L "$p" ]; do
    [ "$n" -lt 40 ] || return 1
    t="$(readlink "$p")" || return 1
    case "$t" in /*) p="$t" ;; *) p="${p%/*}/$t" ;; esac
    n=$((n + 1))
  done
  d="$(cd "${p%/*}/" 2>/dev/null && pwd -P)" || return 1
  [ "$d" = "/" ] && d=""
  printf '%s\n' "$d/${p##*/}"
}

# bin_fresh_is_artifact BIN: succeeds when BIN is a cargo build artifact.
bin_fresh_is_artifact() {
  local dir
  case "$1" in */cargo-target-pool/*) return 0 ;; esac
  dir="${1%/*}"
  while [ -n "$dir" ]; do
    if [ -f "$dir/CACHEDIR.TAG" ] || [ -f "$dir/.rustc_info.json" ]; then return 0; fi
    dir="${dir%/*}"
  done
  return 1
}

# bin_fresh_cargo_pkg BIN: the `cargo install --list` package line owning BIN.
bin_fresh_cargo_pkg() {
  local name="${1##*/}"
  command -v cargo >/dev/null 2>&1 || { echo "cargo not on PATH; source unknown"; return 0; }
  cargo install --list 2>/dev/null | awk -v want="$name" '
    /^[^ ]/ { pkg = $0; sub(/:$/, "", pkg); next }
    { b = $0; gsub(/^ +| +$/, "", b); if (b == want) { print "cargo install: " pkg; found = 1; exit } }
    END { if (!found) print "not listed by cargo install --list" }' || true
}

# bin_fresh_under ROOT: the stdin paths inside ROOT, made ROOT-relative.
bin_fresh_under() {
  local d
  while IFS= read -r d; do
    case "$d" in "$1"/*) printf '%s\n' "${d#"$1"/}" ;; esac
  done
}

# bin_fresh_crate_dirs PACKAGE ROOT USE_OVERRIDE: the root-relative dirs of
# the workspace crates PACKAGE builds from, one per line. USE_OVERRIDE 1 runs
# BIN_FRESH_SOURCES_CMD instead of cargo. Returns 1 with a reason on stderr.
bin_fresh_crate_dirs() {
  local pkg="$1" root="$2" out dirs
  if [ "$3" = 1 ]; then
    out="$(cd "$root" && sh -c "$BIN_FRESH_SOURCES_CMD" 2>&1)" || {
      echo "BIN_FRESH_SOURCES_CMD failed: $out" >&2; return 1; }
  else
    command -v cargo >/dev/null 2>&1 || { echo "cargo not on PATH, so the crates $pkg builds from are unknown" >&2; return 1; }
    out="$(cd "$root" && cargo tree -p "$pkg" -e normal,build --prefix none --format '{p}' --offline --locked 2>&1)" || {
      echo "'cargo tree -p $pkg --offline --locked' failed in $root (a missing offline index or a stale Cargo.lock): $(printf '%s' "$out" | tail -n 2)" >&2
      return 1
    }
  fi
  # bash 3.2 cannot parse a `case` inside `$( )`; the filter is a function.
  dirs="$(printf '%s\n' "$out" | sed -n 's/.*(\(\/[^)]*\)).*/\1/p' | sort -u \
    | bin_fresh_under "$root")"
  [ -n "$dirs" ] || { echo "the dependency tree of $pkg names no crate under $root" >&2; return 1; }
  printf '%s\n' "$dirs"
}

# bin_fresh_unknown PREFIX BIN ALLOW REASON: refuse (or warn under ALLOW).
bin_fresh_unknown() {
  if [ "$3" = 1 ]; then
    echo "$1: warning: --allow-stale: freshness of $2 unknown: $4" >&2
    return 0
  fi
  echo "$1: refused: cannot judge the freshness of $2: $4 (pass --allow-stale to skip)" >&2
  return 1
}

bin_fresh_check() {
  local prefix="$1" link="$2" allow="$3" root="$4" pkg="$5" bin bin_m newest src_m src dirs d cargo_bin use_override
  shift 5
  bin="$(bin_fresh_resolve "$link")" || {
    echo "$prefix: refused: cannot resolve the symlink chain of $link" >&2; return 1; }
  bin_m="$(stat "$BF_STAT_OPT" "$BF_STAT_FMT" "$bin" 2>/dev/null)" || bin_m=""
  bin_m="${bin_m%% *}"
  if [ -z "$bin_m" ]; then
    echo "$prefix: refused: cannot read the mtime of $bin" >&2
    return 1
  fi
  if [ "$bin" = "$link" ]; then
    echo "$prefix: binary: $bin (mtime $(bin_fresh_fmt "$bin_m"))"
  else
    echo "$prefix: binary: $link -> $bin (mtime $(bin_fresh_fmt "$bin_m"))"
  fi

  if ! bin_fresh_is_artifact "$bin"; then
    cargo_bin="${CARGO_HOME:-${HOME:-/nonexistent}/.cargo}/bin"
    if [ "${bin%/*}" = "$cargo_bin" ] || [ "${link%/*}" = "$cargo_bin" ]; then
      echo "$prefix: notice: installed binary, freshness not checked against the checkout; $(bin_fresh_cargo_pkg "$bin")"
    else
      echo "$prefix: notice: installed binary (not under a cargo target dir), freshness not checked against the checkout: $bin"
    fi
    return 0
  fi

  src="$(cd "$root" 2>/dev/null && pwd -P)" || {
    bin_fresh_unknown "$prefix" "$bin" "$allow" "the source root $root is not a directory"; return; }
  root="$src"
  # The override is test-only and never silent (#10755).
  use_override=0
  if [ -n "${BIN_FRESH_SOURCES_CMD:-}" ]; then
    if [ "${BIN_FRESH_TEST:-}" = 1 ]; then
      use_override=1
      echo "$prefix: sources from BIN_FRESH_SOURCES_CMD override: $BIN_FRESH_SOURCES_CMD" >&2
    else
      echo "$prefix: warning: BIN_FRESH_SOURCES_CMD is set but ignored (it needs BIN_FRESH_TEST=1); deriving sources with cargo tree" >&2
    fi
  fi
  dirs="$(bin_fresh_crate_dirs "$pkg" "$root" "$use_override" 2>&1)" || {
    bin_fresh_unknown "$prefix" "$bin" "$allow" "$dirs"; return; }
  set -- Cargo.lock "$@"
  while IFS= read -r d; do
    set -- "$@" "$d/src" "$d/build.rs" "$d/Cargo.toml" "$d/help.yaml" "$d/assets"
  done <<EOF
$dirs
EOF

  # Newest source, untracked new modules included. A tracked file deleted from
  # the worktree fails stat; it is skipped, so the pipeline status is ignored.
  newest="$(cd "$root" && git ls-files -z --cached --others --exclude-standard -- "$@" 2>/dev/null \
    | xargs -0 stat "$BF_STAT_OPT" "$BF_STAT_FMT" 2>/dev/null \
    | sort -n | tail -n 1)" || true
  if [ -z "$newest" ]; then
    bin_fresh_unknown "$prefix" "$bin" "$allow" "no source file of $pkg found by git under $root"
    return
  fi
  src_m="${newest%% *}"
  src="${newest#* }"
  if [ "$bin_m" -ge "$src_m" ]; then
    echo "$prefix: fresh: build artifact is not older than its newest source ($root/$src)"
    return 0
  fi
  if [ "$allow" = 1 ]; then
    echo "$prefix: warning: --allow-stale: $bin (mtime $(bin_fresh_fmt "$bin_m")) is older than $root/$src (mtime $(bin_fresh_fmt "$src_m"))" >&2
    return 0
  fi
  echo "$prefix: refused: stale build artifact $bin (mtime $(bin_fresh_fmt "$bin_m")) is older than its newest source $root/$src (mtime $(bin_fresh_fmt "$src_m")); rebuild, pass --bin <slot>/debug/<bin>, or pass --allow-stale" >&2
  return 1
}
