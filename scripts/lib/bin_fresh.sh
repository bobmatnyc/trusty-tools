#!/usr/bin/env bash
#
# bin_fresh.sh — the sandbox launchers' binary-freshness guard (issue #10755).
#
# Why: a `tm tui` live check ran a stale debug `tm`. Builds land in the build
#   lease's pool slot, so the binary a launcher finds on PATH or under the
#   caller's ambient $CARGO_TARGET_DIR can predate the source under test, and
#   nothing said so.
# What: defines `bin_fresh_check PREFIX BIN ALLOW_STALE SRC_ROOT PATHSPEC...`.
#   BIN must be an absolute path. It always prints BIN's path and mtime.
#   - Build artifact: BIN sits under `.../cargo-target-pool/...`, or below a
#     dir holding `.rustc_info.json` or `CACHEDIR.TAG` with a `debug` or
#     `release` component between that dir and BIN. It is compared with the
#     newest git-tracked file under SRC_ROOT matching the PATHSPECs. Older:
#     returns 1 with a refusal naming both mtimes and that file on stderr,
#     unless ALLOW_STALE is 1, which prints a warning and returns 0. No
#     tracked source found also returns 1, since freshness is then unknown.
#   - Anything else (e.g. ~/.cargo/bin/tm): an installed binary whose mtime
#     says nothing about the checkout. Prints a notice and returns 0. Under
#     the cargo bin dir it adds the package `cargo install --list` names.
#   PATHSPECs should name only what cargo compiles (src/, build.rs,
#   Cargo.toml, Cargo.lock): a changelog fragment does not trigger a rebuild,
#   so counting it would refuse a binary cargo considers current.
# Test: scripts/sandbox_daemon_selftest.sh, scripts/sandbox_code_daemon_selftest.sh
#   (the bin-fresh-* cases).
# Portability: bash 3.2 (macOS) and bash 5 (Linux); BSD and GNU stat/date.

# bin_fresh_fmt EPOCH: a local timestamp for a message.
bin_fresh_fmt() {
  case "$(uname -s)" in
    Darwin) date -r "$1" '+%Y-%m-%d %H:%M:%S' 2>/dev/null || echo "$1" ;;
    *) date -d "@$1" '+%Y-%m-%d %H:%M:%S' 2>/dev/null || echo "$1" ;;
  esac
}

# The stat option and format that print "<mtime> <name>": BSD -f, GNU -c.
case "$(uname -s)" in
  Darwin|*BSD) BF_STAT_OPT=-f; BF_STAT_FMT='%m %N' ;;
  *) BF_STAT_OPT=-c; BF_STAT_FMT='%Y %n' ;;
esac

# bin_fresh_is_artifact BIN: succeeds when BIN is a cargo build artifact.
bin_fresh_is_artifact() {
  local bin="$1" dir
  case "$bin" in */cargo-target-pool/*) return 0 ;; esac
  case "$bin" in */debug/*|*/release/*) ;; *) return 1 ;; esac
  dir="${bin%/*}"
  while [ -n "$dir" ] && [ "$dir" != "/" ]; do
    if [ -f "$dir/.rustc_info.json" ] || [ -f "$dir/CACHEDIR.TAG" ]; then
      case "${bin#"$dir"}" in */debug/*|*/release/*) return 0 ;; esac
    fi
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

bin_fresh_check() {
  local prefix="$1" bin="$2" allow="$3" root="$4" bin_m newest src_m src cargo_bin
  shift 4
  bin_m="$(stat "$BF_STAT_OPT" "$BF_STAT_FMT" "$bin" 2>/dev/null)" || bin_m=""
  bin_m="${bin_m%% *}"
  if [ -z "$bin_m" ]; then
    echo "$prefix: refused: cannot read the mtime of $bin" >&2
    return 1
  fi
  echo "$prefix: binary: $bin (mtime $(bin_fresh_fmt "$bin_m"))"

  if ! bin_fresh_is_artifact "$bin"; then
    cargo_bin="${CARGO_HOME:-${HOME:-/nonexistent}/.cargo}/bin"
    if [ "${bin%/*}" = "$cargo_bin" ]; then
      echo "$prefix: notice: installed binary, freshness not checked against the checkout; $(bin_fresh_cargo_pkg "$bin")"
    else
      echo "$prefix: notice: installed binary (not under a cargo target dir), freshness not checked against the checkout: $bin"
    fi
    return 0
  fi

  src="$(cd "$root" 2>/dev/null && pwd -P)" && root="$src"
  # Newest tracked source. A tracked file deleted from the worktree fails
  # stat; it is skipped, so the pipeline status is ignored.
  newest="$(cd "$root" 2>/dev/null && git ls-files -z -- "$@" 2>/dev/null \
    | xargs -0 stat "$BF_STAT_OPT" "$BF_STAT_FMT" 2>/dev/null \
    | sort -n | tail -n 1)" || true
  if [ -z "$newest" ]; then
    if [ "$allow" = 1 ]; then
      echo "$prefix: warning: --allow-stale: no git-tracked source under $root for [$*]; freshness of $bin unknown" >&2
      return 0
    fi
    echo "$prefix: refused: no git-tracked source under $root for [$*]; cannot judge $bin (pass --allow-stale to skip)" >&2
    return 1
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
