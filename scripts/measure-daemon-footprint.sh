#!/usr/bin/env bash
#
# measure-daemon-footprint.sh — one comparable phys-footprint reading for the
# trusty-search / trusty-memory daemons, plus a breakdown. (#6819)
#
# Why: every child issue of epic #6802 has to prove a before/after memory
#   claim, and each one re-deriving the recipe by hand produces numbers that
#   are not comparable. `ps` RSS is the trap this script exists to avoid: on
#   macOS it undercounts a daemon's real footprint by orders of magnitude
#   (see the `macos-phys-footprint-vs-ps-rss` note), so `phys_footprint` is
#   the only reading worth quoting. The Linux side follows the precedent
#   already in the tree: `RssAnon` from `/proc/<pid>/status`, which is what
#   `TRUSTY_MEMORY_ENFORCE_MEASURE=anon` gates on
#   (`core::memguard_enforce::anon_rss_mb_for_pid`,
#   crates/trusty-search/src/core/memguard_enforce.rs:109).
#
# What: resolves ONE pid, produces ONE primary number, and surrounds it with
#   context an operator can act on.
#     primary   macOS: `footprint -f bytes <pid>` → `phys_footprint`.
#               Falls back to `vmmap -summary <pid>` → `Physical footprint`
#               (coarser: one decimal place, so ~0.5% granularity).
#               Linux: `/proc/<pid>/status` → `RssAnon`, else `VmRSS`.
#     breakdown macOS: the per-category dirty bytes from the same footprint
#               run, plus `phys_footprint_peak`. Linux: RssAnon / RssFile /
#               RssShmem / VmRSS.
#     health    the daemon's own `rss_mb`. trusty-search serves `/health` over
#               loopback TCP; trusty-memory is UDS-only since #6286, so its
#               reading comes from one `memory.health` JSON-RPC frame over the
#               socket. Supplementary — an unreachable daemon does not fail
#               the run, because the pid-level number is the deliverable.
#     disk      the data directory's size. Measured with `du -sk` (portable,
#               machine-readable) and rendered human-readable here, rather
#               than parsing `du -sh`'s locale-dependent output.
#     units     trusty-search: index count, warm-booted (resident) count and
#               total chunks from `/health`. trusty-memory: palace count,
#               LRU-cached count and total drawers from `memory.status`. Every
#               key is emitted for both daemons — `null` where it does not
#               apply — so the JSON key set does not vary with the target.
#
#   Fail-closed: any path that cannot produce the primary number exits 2 with
#   a message naming what was missing. It never prints 0 as if measured.
#
# Test: scripts/measure-daemon-footprint_selftest.sh
#
# Usage:
#   bash scripts/measure-daemon-footprint.sh search
#   bash scripts/measure-daemon-footprint.sh --daemon memory --json
#   bash scripts/measure-daemon-footprint.sh --pid 42054
#
# Options:
#   --daemon <search|memory|trusty-search|trusty-memory>
#   --pid <n>        measure this pid; skips daemon discovery
#   --json           machine-readable output with a stable key set
#   --no-health      skip the daemon's own /health reading
#   --no-disk        skip the data-directory `du` pass
#   -h, --help       this text (exit 0)
#
# Exit: 0 measured. 2 anything else — bad usage, daemon not running, no
#   footprint/vmmap and no /proc, or an unparseable reading.
#
# Environment (test seams, and the overrides the daemons themselves honor):
#   TRUSTY_DATA_DIR_OVERRIDE   absolute data-dir base, as trusty_common::resolve_data_dir reads it
#   TRUSTY_DATA_DIR            trusty-search's own app-dir override (issue #281)
#   MEASURE_FOOTPRINT_PLATFORM force `macos` or `linux` (selftest only)
#   MEASURE_DAEMON_FOOTPRINT_LIB=1 when sourced: define the functions and return
#
# Portability: bash 3.2 (macOS system bash) and bash 5 (Linux), same as the
#   rest of scripts/.

set -uo pipefail

SCHEMA="trusty-footprint/1"
BREAKDOWN_LIMIT=12
HEALTH_TIMEOUT_SECS=5

# ---------------------------------------------------------------------------
# Diagnostics
# ---------------------------------------------------------------------------

die() {
  echo "measure-daemon-footprint: $*" >&2
  exit 2
}

usage() {
  sed -n '3,66p' "$0" | sed 's/^# \{0,1\}//'
}

# ---------------------------------------------------------------------------
# Pure helpers — every one of these is exercised by the selftest
# ---------------------------------------------------------------------------

# detect_platform -> macos | linux | unsupported
detect_platform() {
  if [ -n "${MEASURE_FOOTPRINT_PLATFORM:-}" ]; then
    echo "${MEASURE_FOOTPRINT_PLATFORM}"
    return 0
  fi
  case "$(uname -s)" in
    Darwin) echo macos ;;
    Linux) echo linux ;;
    *) echo unsupported ;;
  esac
}

# canonical_daemon <name> -> trusty-search | trusty-memory; exit 1 if unknown
canonical_daemon() {
  case "$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')" in
    search | trusty-search | ts) echo trusty-search ;;
    memory | trusty-memory | tm-daemon) echo trusty-memory ;;
    *) return 1 ;;
  esac
}

# launchd_label <canonical-daemon> -> com.trusty.<short>
launchd_label() {
  case "$1" in
    trusty-search) echo com.trusty.search ;;
    trusty-memory) echo com.trusty.memory ;;
    *) return 1 ;;
  esac
}

# is_positive_int <string>
is_positive_int() {
  case "$1" in
    '' | *[!0-9]*) return 1 ;;
    0) return 1 ;;
    *) return 0 ;;
  esac
}

# to_bytes <number> <unit>  — unit is one of B K KB M MB G GB T TB (any case).
# Accepts a fractional number (vmmap prints "9.3G"). Emits integer bytes.
to_bytes() {
  local num="$1" unit
  unit="$(printf '%s' "${2:-B}" | tr '[:lower:]' '[:upper:]')"
  local mult
  case "$unit" in
    B | BYTES) mult=1 ;;
    K | KB | KIB) mult=1024 ;;
    M | MB | MIB) mult=1048576 ;;
    G | GB | GIB) mult=1073741824 ;;
    T | TB | TIB) mult=1099511627776 ;;
    *) return 1 ;;
  esac
  case "$num" in
    '' | *[!0-9.]*) return 1 ;;
  esac
  awk -v n="$num" -v m="$mult" 'BEGIN { printf "%.0f\n", n * m }'
}

# mb_from_bytes <bytes> — MiB, truncating, matching /health's rss_mb.
mb_from_bytes() {
  awk -v b="$1" 'BEGIN { printf "%d\n", int(b / 1048576) }'
}

# human_bytes <bytes> — "8.5G"-style, the shape `du -sh` would print.
human_bytes() {
  awk -v b="$1" 'BEGIN {
    split("B K M G T P", u, " ")
    i = 1
    v = b + 0
    while (v >= 1024 && i < 6) { v /= 1024; i++ }
    if (i == 1) printf "%dB\n", v
    else if (v >= 100) printf "%d%s\n", v, u[i]
    else printf "%.1f%s\n", v, u[i]
  }'
}

# json_escape <string> — the only characters these values can carry that JSON
# forbids are the backslash and the double quote (paths, category names).
json_escape() {
  printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
}

# ---------------------------------------------------------------------------
# Parsers — each takes a FILE so the selftest can feed it a fixture
# ---------------------------------------------------------------------------

# parse_footprint_field <file> <field>  — "phys_footprint" or
# "phys_footprint_peak" from `footprint`'s "Auxiliary data:" block. `-f bytes`
# makes the unit always "B", but the unit is parsed rather than assumed so a
# `formatted` run still works.
parse_footprint_field() {
  local file="$1" field="$2" line num unit
  line="$(grep -E "^[[:space:]]*${field}:" "$file" 2>/dev/null | head -1)"
  [ -n "$line" ] || return 1
  num="$(printf '%s' "$line" | awk '{ print $2 }')"
  unit="$(printf '%s' "$line" | awk '{ print $3 }')"
  [ -n "$num" ] || return 1
  to_bytes "$num" "${unit:-B}"
}

# parse_footprint_categories <file> — "<dirty-bytes>\t<category>", biggest
# first, zero-dirty and the TOTAL row dropped. Only meaningful for a
# `-f bytes` run, which is what this script always requests.
parse_footprint_categories() {
  awk '
    $2 == "B" && $1 + 0 > 0 {
      cat = $8
      for (i = 9; i <= NF; i++) cat = cat " " $i
      if (cat == "TOTAL" || cat == "") next
      printf "%d\t%s\n", $1, cat
    }
  ' "$1" | sort -rn -k1,1
}

# parse_vmmap_footprint <file> — `vmmap -summary`'s "Physical footprint:".
# Coarser than `footprint` (one decimal), used only when footprint is absent.
parse_vmmap_footprint() {
  local file="$1" want="${2:-Physical footprint}" raw num unit
  # Literal prefix match, not a regex: the label this is asked for is
  # "Physical footprint (peak)", whose parentheses an ERE would read as a
  # group and silently match the wrong line.
  raw="$(awk -v want="${want}:" 'index($0, want) == 1 { print $NF; exit }' \
    "$file" 2>/dev/null)"
  [ -n "$raw" ] || return 1
  num="$(printf '%s' "$raw" | sed 's/[^0-9.]//g')"
  unit="$(printf '%s' "$raw" | sed 's/[0-9.]//g')"
  [ -n "$num" ] || return 1
  to_bytes "$num" "${unit:-B}"
}

# parse_proc_status_kb <file> <field> — one kB field from /proc/<pid>/status.
# `RssAnon` is the primary reading, per the same choice
# `core::memguard_enforce::anon_rss_mb_for_pid` makes; `VmRSS` is the
# fallback for a kernel or container that does not expose RssAnon.
parse_proc_status_kb() {
  local file="$1" field="$2" val
  val="$(grep -E "^${field}:" "$file" 2>/dev/null | head -1 | awk '{ print $2 }')"
  is_positive_int "$val" || return 1
  echo "$val"
}

# ---------------------------------------------------------------------------
# Discovery
# ---------------------------------------------------------------------------

pid_is_alive() {
  is_positive_int "$1" || return 1
  kill -0 "$1" 2>/dev/null
}

# resolve_pid_launchd <label>
resolve_pid_launchd() {
  local label="$1" pid
  command -v launchctl >/dev/null 2>&1 || return 1
  pid="$(launchctl print "gui/$(id -u)/${label}" 2>/dev/null |
    awk -F'= ' '/^[[:space:]]*pid = /{ print $2; exit }')"
  is_positive_int "$pid" || return 1
  echo "$pid"
}

# resolve_pid_pgrep <canonical-daemon> — fail closed on ambiguity. On a
# developer box `pgrep -f trusty-memory` also matches every MCP stdio client
# and every worktree's debug build, so guessing among them would report the
# wrong process's footprint as the daemon's.
resolve_pid_pgrep() {
  local daemon="$1" pids count
  command -v pgrep >/dev/null 2>&1 || return 1
  pids="$(pgrep -f "${daemon}.*--foreground" 2>/dev/null)"
  count="$(printf '%s\n' "$pids" | grep -c '[0-9]' || true)"
  if [ "$count" != "1" ]; then
    pids="$(pgrep -x "$daemon" 2>/dev/null)"
    count="$(printf '%s\n' "$pids" | grep -c '[0-9]' || true)"
  fi
  if [ "$count" = "1" ]; then
    printf '%s\n' "$pids" | tr -d '[:space:]'
    return 0
  fi
  PGREP_CANDIDATES="$(printf '%s' "$pids" | tr '\n' ' ')"
  return 1
}

# data_dir_base — where trusty_common::resolve_data_dir puts an app's dir.
data_dir_base() {
  local platform="$1"
  if [ -n "${TRUSTY_DATA_DIR_OVERRIDE:-}" ]; then
    case "${TRUSTY_DATA_DIR_OVERRIDE}" in
      /*)
        echo "${TRUSTY_DATA_DIR_OVERRIDE}"
        return 0
        ;;
    esac
  fi
  if [ "$platform" = "macos" ]; then
    echo "${HOME}/Library/Application Support"
  else
    echo "${XDG_DATA_HOME:-${HOME}/.local/share}"
  fi
}

# daemon_data_subdir <canonical-daemon> <platform> — the directory whose size
# is the daemon's on-disk corpus: indexes for search, palaces for memory.
daemon_data_subdir() {
  local daemon="$1" platform="$2" base leaf
  case "$daemon" in
    trusty-search) leaf=indexes ;;
    trusty-memory) leaf=palaces ;;
    *) return 1 ;;
  esac
  if [ "$daemon" = "trusty-search" ] && [ -n "${TRUSTY_DATA_DIR:-}" ]; then
    echo "${TRUSTY_DATA_DIR}/${leaf}"
    return 0
  fi
  base="$(data_dir_base "$platform")"
  echo "${base}/${daemon}/${leaf}"
}

# search_health_url — the address the running daemon published, else the
# compiled-in default.
search_health_url() {
  local platform="$1" base addr f
  base="$(data_dir_base "$platform")"
  for f in "${TRUSTY_DATA_DIR:-${base}/trusty-search}/http_addr" "${HOME}/.trusty-search/http_addr"; do
    if [ -r "$f" ]; then
      addr="$(tr -d '[:space:]' <"$f")"
      if [ -n "$addr" ]; then
        echo "http://${addr}/health"
        return 0
      fi
    fi
  done
  echo "http://127.0.0.1:7878/health"
}

# memory_socket_path <platform>
memory_socket_path() {
  local base
  base="$(data_dir_base "$1")"
  echo "${base}/trusty-memory/trusty-memory.sock"
}

# uds_rpc <socket> <method> — one newline-terminated JSON-RPC frame, which is
# exactly what trusty_common::uds::server reads (crates/trusty-common/src/uds/
# server/mod.rs). Prints the raw response frame.
uds_rpc() {
  local sock="$1" method="$2"
  [ -S "$sock" ] || return 1
  command -v nc >/dev/null 2>&1 || return 1
  printf '{"jsonrpc":"2.0","id":1,"method":"%s","params":{}}\n' "$method" |
    nc -U -w "$HEALTH_TIMEOUT_SECS" "$sock" 2>/dev/null
}

# ---------------------------------------------------------------------------
# Measurement
# ---------------------------------------------------------------------------

# Outputs, set by measure_*: PRIMARY_BYTES, PEAK_BYTES ("" when unknown),
# SOURCE, BREAKDOWN_FILE.
measure_macos() {
  local pid="$1" out
  out="${WORKDIR}/footprint.txt"
  if command -v footprint >/dev/null 2>&1 &&
    footprint -f bytes "$pid" >"$out" 2>/dev/null &&
    PRIMARY_BYTES="$(parse_footprint_field "$out" phys_footprint)"; then
    SOURCE="footprint"
    PEAK_BYTES="$(parse_footprint_field "$out" phys_footprint_peak || true)"
    parse_footprint_categories "$out" | head -"$BREAKDOWN_LIMIT" >"$BREAKDOWN_FILE"
    return 0
  fi
  out="${WORKDIR}/vmmap.txt"
  if command -v vmmap >/dev/null 2>&1 &&
    vmmap -summary "$pid" >"$out" 2>/dev/null &&
    PRIMARY_BYTES="$(parse_vmmap_footprint "$out" "Physical footprint")"; then
    SOURCE="vmmap-summary"
    PEAK_BYTES="$(parse_vmmap_footprint "$out" "Physical footprint (peak)" || true)"
    : >"$BREAKDOWN_FILE"
    return 0
  fi
  return 1
}

measure_linux() {
  local pid="$1" status kb
  status="${MEASURE_FOOTPRINT_PROC_STATUS:-/proc/${pid}/status}"
  [ -r "$status" ] || return 1
  : >"$BREAKDOWN_FILE"
  if kb="$(parse_proc_status_kb "$status" RssAnon)"; then
    SOURCE="proc-status-rssanon"
  elif kb="$(parse_proc_status_kb "$status" VmRSS)"; then
    SOURCE="proc-status-vmrss"
  else
    return 1
  fi
  PRIMARY_BYTES="$((kb * 1024))"
  PEAK_BYTES="$(parse_proc_status_kb "$status" VmHWM 2>/dev/null || true)"
  [ -n "$PEAK_BYTES" ] && PEAK_BYTES="$((PEAK_BYTES * 1024))"
  local field
  for field in RssAnon RssFile RssShmem VmRSS; do
    if kb="$(parse_proc_status_kb "$status" "$field")"; then
      printf '%d\t%s\n' "$((kb * 1024))" "$field" >>"$BREAKDOWN_FILE"
    fi
  done
  return 0
}

# read_health <canonical-daemon> <platform> — sets HEALTH_RSS_MB,
# HEALTH_ENDPOINT and the UNIT_* counts. Every UNIT_* key is emitted for both
# daemons, `null` where it does not apply, so the JSON key set never varies
# with which daemon was measured.
# Returns 1 when the daemon could not be reached, which is not fatal: the
# pid-level phys_footprint is the deliverable, this is context around it.
#
# trusty-memory's counts come from `memory.status`, not `memory.palaces_list`.
# The list call opens every palace and took ~9s on a 93-palace host, which is
# both slower than the socket read deadline and more work than a measurement
# pass should ask a live daemon to do; `memory.status` answers instantly.
read_health() {
  local daemon="$1" platform="$2" body sock
  command -v jq >/dev/null 2>&1 || return 1
  if [ "$daemon" = "trusty-search" ]; then
    HEALTH_ENDPOINT="$(search_health_url "$platform")"
    command -v curl >/dev/null 2>&1 || return 1
    body="$(curl -sS --max-time "$HEALTH_TIMEOUT_SECS" "$HEALTH_ENDPOINT" 2>/dev/null)"
    [ -n "$body" ] || return 1
    HEALTH_RSS_MB="$(printf '%s' "$body" | jq -r '.rss_mb // empty' 2>/dev/null)"
    UNIT_KIND="indexes"
    UNIT_COUNT="$(printf '%s' "$body" | jq -r '.indexes // empty' 2>/dev/null)"
    UNIT_CHUNKS="$(printf '%s' "$body" | jq -r '.total_chunks // empty' 2>/dev/null)"
    UNIT_DRAWERS=""
    UNIT_RESIDENT="$(printf '%s' "$body" |
      jq -r '.warmboot_summary.indexes_loaded // empty' 2>/dev/null)"
  else
    sock="$(memory_socket_path "$platform")"
    HEALTH_ENDPOINT="${sock} (memory.health)"
    body="$(uds_rpc "$sock" memory.health)" || return 1
    [ -n "$body" ] || return 1
    HEALTH_RSS_MB="$(printf '%s' "$body" | jq -r '.result.rss_mb // empty' 2>/dev/null)"
    UNIT_KIND="palaces"
    body="$(uds_rpc "$sock" memory.status 2>/dev/null || true)"
    UNIT_COUNT="$(printf '%s' "$body" | jq -r '.result.palace_count // empty' 2>/dev/null)"
    UNIT_CHUNKS=""
    UNIT_DRAWERS="$(printf '%s' "$body" | jq -r '.result.total_drawers // empty' 2>/dev/null)"
    UNIT_RESIDENT="$(printf '%s' "$body" |
      jq -r '.result.cached_palace_count // empty' 2>/dev/null)"
  fi
  is_positive_int "$HEALTH_RSS_MB" || HEALTH_RSS_MB=""
  is_positive_int "$UNIT_COUNT" || UNIT_COUNT=""
  is_positive_int "$UNIT_CHUNKS" || UNIT_CHUNKS=""
  is_positive_int "$UNIT_DRAWERS" || UNIT_DRAWERS=""
  is_positive_int "$UNIT_RESIDENT" || UNIT_RESIDENT=""
  [ -n "$HEALTH_RSS_MB" ] || return 1
  return 0
}

# ---------------------------------------------------------------------------
# Output
# ---------------------------------------------------------------------------

emit_human() {
  printf '%s  pid %s  (%s, source=%s)\n\n' "$DAEMON" "$PID" "$PLATFORM" "$SOURCE"
  printf '  %-24s %10s MB  (%s B)\n' "phys_footprint" \
    "$(mb_from_bytes "$PRIMARY_BYTES")" "$PRIMARY_BYTES"
  if [ -n "$PEAK_BYTES" ]; then
    printf '  %-24s %10s MB\n' "phys_footprint_peak" "$(mb_from_bytes "$PEAK_BYTES")"
  fi
  if [ -n "$HEALTH_RSS_MB" ]; then
    printf '  %-24s %10s MB  (%s)\n' "/health rss_mb" "$HEALTH_RSS_MB" "$HEALTH_ENDPOINT"
  elif [ "$WANT_HEALTH" = "1" ]; then
    printf '  %-24s %10s\n' "/health rss_mb" "unavailable"
  fi
  if [ -n "$DISK_KB" ]; then
    printf '  %-24s %10s      %s\n' "data dir" \
      "$(human_bytes "$((DISK_KB * 1024))")" "$DATA_DIR"
  fi
  if [ -n "$UNIT_COUNT" ]; then
    printf '  %-24s %10s      resident %s\n' "$UNIT_KIND" "$UNIT_COUNT" \
      "${UNIT_RESIDENT:-?}"
  fi
  if [ -n "$UNIT_CHUNKS" ]; then
    printf '  %-24s %10s\n' "chunks" "$UNIT_CHUNKS"
  fi
  if [ -n "$UNIT_DRAWERS" ]; then
    printf '  %-24s %10s\n' "drawers" "$UNIT_DRAWERS"
  fi
  if [ -s "$BREAKDOWN_FILE" ]; then
    printf '\n  breakdown:\n'
    while IFS="$(printf '\t')" read -r bytes cat; do
      [ -n "$bytes" ] || continue
      printf '    %-22s %10s MB\n' "$cat" "$(mb_from_bytes "$bytes")"
    done <"$BREAKDOWN_FILE"
  fi
}

# json_num <value> — a JSON number, or `null` when the reading is absent. A
# missing reading must never render as 0.
json_num() {
  if [ -n "${1:-}" ]; then printf '%s' "$1"; else printf 'null'; fi
}

json_str() {
  if [ -n "${1:-}" ]; then printf '"%s"' "$(json_escape "$1")"; else printf 'null'; fi
}

emit_json() {
  local first=1 bytes cat
  printf '{\n'
  printf '  "schema": %s,\n' "$(json_str "$SCHEMA")"
  printf '  "daemon": %s,\n' "$(json_str "$DAEMON")"
  printf '  "pid": %s,\n' "$(json_num "$PID")"
  printf '  "platform": %s,\n' "$(json_str "$PLATFORM")"
  printf '  "measured_at": %s,\n' "$(json_str "$MEASURED_AT")"
  printf '  "source": %s,\n' "$(json_str "$SOURCE")"
  printf '  "phys_footprint_bytes": %s,\n' "$(json_num "$PRIMARY_BYTES")"
  printf '  "phys_footprint_mb": %s,\n' "$(json_num "$(mb_from_bytes "$PRIMARY_BYTES")")"
  if [ -n "$PEAK_BYTES" ]; then
    printf '  "phys_footprint_peak_mb": %s,\n' "$(json_num "$(mb_from_bytes "$PEAK_BYTES")")"
  else
    printf '  "phys_footprint_peak_mb": null,\n'
  fi
  printf '  "health": { "rss_mb": %s, "endpoint": %s },\n' \
    "$(json_num "$HEALTH_RSS_MB")" "$(json_str "$HEALTH_ENDPOINT")"
  printf '  "data_dir": { "path": %s, "size_kb": %s, "size_human": %s },\n' \
    "$(json_str "$DATA_DIR")" "$(json_num "$DISK_KB")" \
    "$(if [ -n "$DISK_KB" ]; then json_str "$(human_bytes "$((DISK_KB * 1024))")"; else printf 'null'; fi)"
  printf '  "units": { "kind": %s, "count": %s, "resident": %s, "chunks": %s, "drawers": %s },\n' \
    "$(json_str "$UNIT_KIND")" "$(json_num "$UNIT_COUNT")" \
    "$(json_num "$UNIT_RESIDENT")" "$(json_num "$UNIT_CHUNKS")" \
    "$(json_num "$UNIT_DRAWERS")"
  printf '  "breakdown": ['
  if [ -s "$BREAKDOWN_FILE" ]; then
    printf '\n'
    while IFS="$(printf '\t')" read -r bytes cat; do
      [ -n "$bytes" ] || continue
      [ "$first" = "1" ] || printf ',\n'
      first=0
      printf '    { "category": %s, "bytes": %s, "mb": %s }' \
        "$(json_str "$cat")" "$bytes" "$(mb_from_bytes "$bytes")"
    done <"$BREAKDOWN_FILE"
    printf '\n  '
  fi
  printf ']\n}\n'
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

main() {
  local want_daemon="" want_pid="" as_json=0
  WANT_HEALTH=1
  local want_disk=1

  while [ $# -gt 0 ]; do
    case "$1" in
      --daemon)
        [ $# -ge 2 ] || die "--daemon needs a value"
        want_daemon="$2"
        shift 2
        ;;
      --daemon=*)
        want_daemon="${1#--daemon=}"
        shift
        ;;
      --pid)
        [ $# -ge 2 ] || die "--pid needs a value"
        want_pid="$2"
        shift 2
        ;;
      --pid=*)
        want_pid="${1#--pid=}"
        shift
        ;;
      --json)
        as_json=1
        shift
        ;;
      --no-health)
        WANT_HEALTH=0
        shift
        ;;
      --no-disk)
        want_disk=0
        shift
        ;;
      -h | --help)
        usage
        exit 0
        ;;
      -*) die "unknown option: $1 (see --help)" ;;
      *)
        [ -z "$want_daemon" ] || die "daemon named twice: '$want_daemon' and '$1'"
        want_daemon="$1"
        shift
        ;;
    esac
  done

  PLATFORM="$(detect_platform)"
  case "$PLATFORM" in
    macos | linux) ;;
    *) die "unsupported platform '$(uname -s)': no footprint/vmmap and no /proc/<pid>/status" ;;
  esac

  if [ -n "$want_daemon" ]; then
    DAEMON="$(canonical_daemon "$want_daemon")" ||
      die "unknown daemon '$want_daemon' (expected search or memory)"
  elif [ -n "$want_pid" ]; then
    DAEMON="pid-${want_pid}"
  else
    die "name a daemon (search|memory) or pass --pid <n>; see --help"
  fi

  if [ -n "$want_pid" ]; then
    is_positive_int "$want_pid" || die "--pid must be a positive integer, got '$want_pid'"
    pid_is_alive "$want_pid" || die "pid ${want_pid} is not running (or not visible to this uid)"
    PID="$want_pid"
  else
    PGREP_CANDIDATES=""
    PID="$(resolve_pid_launchd "$(launchd_label "$DAEMON")" || true)"
    if [ -z "$PID" ]; then
      PID="$(resolve_pid_pgrep "$DAEMON" || true)"
    fi
    if [ -z "$PID" ]; then
      if [ -n "${PGREP_CANDIDATES:-}" ]; then
        die "could not pick ${DAEMON}'s daemon pid unambiguously; candidates:${PGREP_CANDIDATES% } — rerun with --pid <n>"
      fi
      die "${DAEMON} is not running (no launchd job and no matching process)"
    fi
    pid_is_alive "$PID" || die "${DAEMON} pid ${PID} is not running"
  fi

  WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/measure-footprint.XXXXXX")" ||
    die "could not create a scratch directory"
  trap 'rm -rf "${WORKDIR}"' EXIT
  BREAKDOWN_FILE="${WORKDIR}/breakdown.tsv"
  : >"$BREAKDOWN_FILE"

  PRIMARY_BYTES=""
  PEAK_BYTES=""
  SOURCE=""
  HEALTH_RSS_MB=""
  HEALTH_ENDPOINT=""
  UNIT_KIND=""
  UNIT_COUNT=""
  UNIT_RESIDENT=""
  UNIT_CHUNKS=""
  UNIT_DRAWERS=""
  DISK_KB=""
  DATA_DIR=""
  MEASURED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

  if [ "$PLATFORM" = "macos" ]; then
    measure_macos "$PID" ||
      die "neither footprint nor vmmap produced a phys_footprint for pid ${PID}"
  else
    measure_linux "$PID" ||
      die "no RssAnon or VmRSS in /proc/${PID}/status"
  fi
  is_positive_int "$PRIMARY_BYTES" ||
    die "phys-footprint reading for pid ${PID} came back empty or zero; refusing to report it"

  case "$DAEMON" in
    trusty-search | trusty-memory)
      if [ "$WANT_HEALTH" = "1" ]; then
        read_health "$DAEMON" "$PLATFORM" || true
      fi
      DATA_DIR="$(daemon_data_subdir "$DAEMON" "$PLATFORM")"
      if [ "$want_disk" = "1" ] && [ -d "$DATA_DIR" ]; then
        DISK_KB="$(du -sk "$DATA_DIR" 2>/dev/null | awk '{ print $1 }')"
        is_positive_int "$DISK_KB" || DISK_KB=""
      fi
      ;;
  esac

  if [ "$as_json" = "1" ]; then
    emit_json
  else
    emit_human
  fi
}

# Library mode: `MEASURE_DAEMON_FOOTPRINT_LIB=1 . scripts/measure-daemon-footprint.sh`
# defines every function above and returns without measuring anything, which is
# how the selftest exercises the parsers with no live daemon. (#6819)
if [ "${MEASURE_DAEMON_FOOTPRINT_LIB:-0}" != "1" ]; then
  main "$@"
fi
