#!/usr/bin/env bash
#
# memory_perf_collect.sh — read-only storage / indexing / effectiveness collector
# for trusty-memory (perf plan gap 1).
#
# Why: the standing perf analysis (docs/trusty-memory/perf/README.md) needs the
#   same live numbers every day, taken without touching the data. A recall call
#   is not read-only (`drawer_access` feeds ranking), and a palace open grows
#   residency, so the collector uses only file metadata, `palace stats` and one
#   `memory.status` call.
# What: samples the 1-minute load average, then reads (a) per-file byte sizes of
#   each sampled palace and `du -sk` of the data root, (b) `trusty-memory palace
#   stats <name> --json` for the N largest palaces, one at a time, (c)
#   `memory.status` over the daemon socket (newline-framed JSON-RPC), and (d)
#   each sampled palace's `dream_stats.json`. It then samples load again and
#   prints one JSON envelope (schema 1) on stdout, and to --out FILE if given.
#   READ-ONLY: it never calls memory_recall, memory_recall_all,
#   palace_embed_sweep or any write, compact, dream or reembed verb, and never
#   starts, stops or restarts a daemon.
#   PUBLIC OUTPUT: palaces are labelled by size rank (p01, p02, ...). Paths are
#   reduced to file basenames. The host is `label`, OS name and version, CPU
#   model and RAM GB only.
#   Idle gate: load >= 2 before the run is a refusal (exit 3). Load >= 2 after
#   marks the output `idle_gate: "failed"`; discard that run. --skip-idle-gate
#   is for sandbox checks and marks the output `idle_gate: "skipped"`.
#
# Usage: scripts/memory_perf_collect.sh [--label mac|evo] [--top N]
#          [--data-root DIR] [--socket PATH] [--bin PATH] [--out FILE]
#          [--commit SHA] [--tool-version V] [--corpus NAME] [--skip-idle-gate]
#   --label           mac or evo (default: mac on Darwin, evo on Linux)
#   --top N           palaces sampled, largest by disk bytes first (default 10)
#   --data-root DIR   trusty-memory data root, the dir that holds the palaces
#                     (env MEMORY_PERF_DATA_ROOT; default: TRUSTY_DATA_DIR_OVERRIDE/
#                     trusty-memory, else the per-user live location). Its last
#                     component must be `trusty-memory`.
#   --socket PATH     daemon socket (env MEMORY_PERF_SOCKET; default
#                     <data-root>/trusty-memory.sock)
#   --bin PATH        trusty-memory binary (env MEMORY_PERF_BIN; default PATH)
#   --out FILE        also write the JSON to FILE
#   --commit SHA      commit recorded in the envelope (default: HEAD of the
#                     checkout that holds this script)
#   --tool-version V  version recorded (default: the daemon's, else "unknown")
#   --corpus NAME     envelope `corpus` (default "live")
# Exit: 0 collected; 2 usage error or missing tool; 3 refused (load >= 2).
#
# Test: manual — run against a scripts/sandbox_memory_daemon.sh daemon with the
#   sandbox's data dir and socket; see docs/trusty-memory/perf/README.md.
# Portability: bash 3.2 (macOS) and bash 5 (Linux).

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

IDLE_MAX=2
TOP=10
LABEL=""
DATA_ROOT="${MEMORY_PERF_DATA_ROOT:-}"
SOCKET="${MEMORY_PERF_SOCKET:-}"
BIN="${MEMORY_PERF_BIN:-trusty-memory}"
OUT=""
COMMIT=""
TOOL_VERSION=""
CORPUS="live"
SKIP_GATE=0
OS="$(uname -s)"

die() { echo "memory_perf_collect: $*" >&2; exit "${2:-2}"; }
usage() { sed -n '/^# Usage:/,/^# Exit:/p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2; }

while [ $# -gt 0 ]; do
  case "$1" in
    --label) LABEL="${2:?--label needs mac|evo}"; shift 2 ;;
    --top) TOP="${2:?--top needs a number}"; shift 2 ;;
    --data-root) DATA_ROOT="${2:?--data-root needs a dir}"; shift 2 ;;
    --socket) SOCKET="${2:?--socket needs a path}"; shift 2 ;;
    --bin) BIN="${2:?--bin needs a path}"; shift 2 ;;
    --out) OUT="${2:?--out needs a file}"; shift 2 ;;
    --commit) COMMIT="${2:?--commit needs a sha}"; shift 2 ;;
    --tool-version) TOOL_VERSION="${2:?--tool-version needs a value}"; shift 2 ;;
    --corpus) CORPUS="${2:?--corpus needs a name}"; shift 2 ;;
    --skip-idle-gate) SKIP_GATE=1; shift ;;
    -h|--help) usage ;;
    *) echo "memory_perf_collect: unknown argument: $1" >&2; usage ;;
  esac
done

case "$TOP" in ''|*[!0-9]*) die "--top must be a positive integer" ;; esac
[ "$TOP" -ge 1 ] || die "--top must be a positive integer"
if [ -z "$LABEL" ]; then
  if [ "$OS" = Darwin ]; then LABEL=mac; else LABEL=evo; fi
fi
case "$LABEL" in mac|evo) ;; *) die "--label must be mac or evo" ;; esac

for tool in jq nc du stat awk sort; do
  command -v "$tool" >/dev/null 2>&1 || die "required tool missing: $tool"
done
command -v "$BIN" >/dev/null 2>&1 || [ -x "$BIN" ] || die "trusty-memory binary not found (use --bin)"

# Default data root: the same resolution trusty-common uses, built from $HOME.
if [ -z "$DATA_ROOT" ]; then
  if [ -n "${TRUSTY_DATA_DIR_OVERRIDE:-}" ]; then
    DATA_ROOT="$TRUSTY_DATA_DIR_OVERRIDE/trusty-memory"
  elif [ "$OS" = Darwin ]; then
    DATA_ROOT="$HOME/Library/Application Support/trusty-memory"
  else
    DATA_ROOT="${XDG_DATA_HOME:-$HOME/.local/share}/trusty-memory"
  fi
fi
[ -d "$DATA_ROOT" ] || die "data root is not a directory (use --data-root)"
case "$DATA_ROOT" in */trusty-memory) ;; *) die "--data-root must end in /trusty-memory" ;; esac
[ -n "$SOCKET" ] || SOCKET="$DATA_ROOT/trusty-memory.sock"
# `palace stats` resolves the root itself; point it at the root we were given.
export TRUSTY_DATA_DIR_OVERRIDE="${DATA_ROOT%/trusty-memory}"
PALACES="$DATA_ROOT"
[ -d "$DATA_ROOT/palaces" ] && PALACES="$DATA_ROOT/palaces"

if [ -z "$COMMIT" ]; then
  COMMIT="$(git -C "$(dirname "$0")" rev-parse HEAD 2>/dev/null || echo unknown)"
fi

SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/memperf.XXXXXX")"
trap 'rm -rf "$SCRATCH"' EXIT
ROWS="$SCRATCH/rows.ndjson"
: > "$ROWS"

load1() {
  if [ "$OS" = Darwin ]; then sysctl -n vm.loadavg | awk '{print $2}'
  else awk '{print $1}' /proc/loadavg; fi
}
ge() { awk -v a="$1" -v b="$2" 'BEGIN { exit !(a + 0 >= b + 0) }'; }
file_bytes() { if [ "$OS" = Darwin ]; then stat -f %z "$1"; else stat -c %s "$1"; fi; }

LOAD_BEFORE="$(load1)"
if [ "$SKIP_GATE" -eq 0 ] && ge "$LOAD_BEFORE" "$IDLE_MAX"; then
  die "refused: 1-minute load average is $LOAD_BEFORE (>= $IDLE_MAX); rerun when idle" 3
fi

# row DIM METRIC UNIT N VALUE(json) NOTES [PALACE] [FILE] [THRESHOLD]
row() {
  jq -cn --arg dimension "$1" --arg metric "$2" --arg unit "$3" --argjson n "$4" \
    --argjson value "$5" --arg notes "$6" --arg palace "${7:-}" --arg file "${8:-}" \
    --arg threshold "${9:-}" '
    {dimension: $dimension, metric: $metric, unit: $unit, n: $n, k: null, p50: null,
     p95: null, max: null, value: $value, threshold: (if $threshold == "" then null else $threshold end),
     notes: $notes}
    + (if $palace == "" then {} else {palace: $palace} end)
    + (if $file == "" then {} else {file: $file} end)' >> "$ROWS"
}

# --- host --------------------------------------------------------------------
if [ "$OS" = Darwin ]; then
  HOST_OS="macOS $(sw_vers -productVersion)"
  HOST_CPU="$(sysctl -n machdep.cpu.brand_string)"
  HOST_RAM_GB="$(sysctl -n hw.memsize | awk '{printf "%d", $1 / 1073741824 + 0.5}')"
else
  # shellcheck source=/dev/null
  HOST_OS="$(. /etc/os-release 2>/dev/null && echo "${NAME:-Linux} ${VERSION_ID:-}" || echo Linux)"
  HOST_CPU="$(awk -F': ' '/model name/ {print $2; exit}' /proc/cpuinfo)"
  HOST_RAM_GB="$(awk '/MemTotal/ {printf "%d", $2 / 1048576 + 0.5}' /proc/meminfo)"
fi

# --- (d) memory.status: residency and totals ----------------------------------
STATUS="null"
if [ -S "$SOCKET" ]; then
  REPLY="$(printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"memory.status","params":{}}' |
    nc -U -w 5 "$SOCKET" 2>/dev/null || true)"
  STATUS="$(printf '%s' "$REPLY" | jq -c '.result // null' 2>/dev/null || echo null)"
  [ -n "$STATUS" ] || STATUS="null"
fi
if [ "$STATUS" != null ]; then
  [ -n "$TOOL_VERSION" ] || TOOL_VERSION="$(jq -r '.version // empty' <<<"$STATUS")"
  RESIDENCY="$(jq -c '{cached: .cached_palace_count, total: .palace_count}' <<<"$STATUS")"
  for m in total_drawers total_vectors total_kg_triples; do
    row storage "$m" count 1 "$(jq -c ".$m // null" <<<"$STATUS")" "memory.status"
  done
else
  RESIDENCY='{"cached":null,"total":null}'
fi
[ -n "$TOOL_VERSION" ] || TOOL_VERSION=unknown

# --- (b) data root bytes -------------------------------------------------------
ROOT_KB="$(du -sk "$DATA_ROOT" | awk '{print $1}')"
row storage data_root_disk_bytes bytes 1 "$((ROOT_KB * 1024))" "du -sk of the data root, all palaces and backups"

# --- rank palaces by disk bytes ------------------------------------------------
LIST="$SCRATCH/palaces.tsv"
: > "$LIST"
TOTAL_PALACES=0
for d in "$PALACES"/*/; do
  d="${d%/}"
  [ -f "$d/palace.json" ] || continue
  TOTAL_PALACES=$((TOTAL_PALACES + 1))
  printf '%s\t%s\n' "$(du -sk "$d" | awk '{print $1}')" "$(basename "$d")" >> "$LIST"
done
row storage palaces_on_disk count 1 "$TOTAL_PALACES" "directories holding a palace.json"
sort -t "$(printf '\t')" -k1,1nr -k2,2 "$LIST" | head -n "$TOP" > "$SCRATCH/top.tsv"

RANK=0
while IFS="$(printf '\t')" read -r _kb dirname; do
  RANK=$((RANK + 1))
  P="$(printf 'p%02d' "$RANK")"
  PDIR="$PALACES/$dirname"

  # (b) per-file bytes; only fixed system basenames are emitted.
  PALACE_BYTES=0
  INDEX_BYTES=0
  OTHER=0
  for f in "$PDIR"/* "$PDIR"/.[!.]*; do
    [ -e "$f" ] || continue
    base="$(basename "$f")"
    if [ -d "$f" ]; then
      b=$(($(du -sk "$f" | awk '{print $1}') * 1024))
    else
      b="$(file_bytes "$f")"
    fi
    PALACE_BYTES=$((PALACE_BYTES + b))
    case "$base" in
      *[!A-Za-z0-9._-]*) OTHER=$((OTHER + b)); continue ;;
    esac
    [ -d "$f" ] && base="$base/"
    [ "$base" = index.usearch.redb ] && INDEX_BYTES="$b"
    row storage file_bytes bytes 1 "$b" "per-file size" "$P" "$base"
  done
  [ "$OTHER" -eq 0 ] || row storage file_bytes bytes 1 "$OTHER" "files with non-standard names" "$P" "other"
  row storage palace_total_bytes bytes 1 "$PALACE_BYTES" "all files in the palace dir" "$P" "" "> 500 MB per palace"

  # (c) palace stats, read serially. Failure is a row, not an abort.
  DRAWERS=0
  if STATS="$("$BIN" palace stats "$(jq -r '.id // empty' "$PDIR/palace.json" 2>/dev/null || echo "$dirname")" --json 2>/dev/null)" &&
     jq -e . >/dev/null 2>&1 <<<"$STATS"; then
    DRAWERS="$(jq '[.tables[] | select(.name == "drawers") | .rows] | add // 0' <<<"$STATS")"
    SNAP="$(jq -r 'if .from_snapshot then "read from a snapshot (a writer held the file)" else "read in place" end' <<<"$STATS")"
    row storage kg_triples_active rows 1 "$(jq .triples_active <<<"$STATS")" "$SNAP" "$P"
    row storage kg_triples_history rows 1 "$(jq .triples_history <<<"$STATS")" "hist: rows, ever" "$P"
    row storage kg_triples_history_stale rows 1 "$(jq .triples_history_stale <<<"$STATS")" "closed more than $(jq .history_cutoff_days <<<"$STATS") days ago" "$P"
    row storage superseded_drawers count 1 "$(jq .superseded_drawers <<<"$STATS")" "active superseded_by triples" "$P"
    row storage drawers count 1 "$DRAWERS" "rows in the kg.redb drawers table" "$P"
    row storage kg_reclaimable_bytes bytes 1 "$(jq .reclaimable_bytes <<<"$STATS")" "compaction estimate" "$P" "" "> 25% of kg.redb"
    row storage kg_reclaimable_ratio ratio 1 "$(jq '(.reclaimable_bytes / (if .file_bytes > 0 then .file_bytes else 1 end))' <<<"$STATS")" "reclaimable_bytes / kg.redb file_bytes" "$P" "" "> 0.25"
    if [ "$DRAWERS" -gt 0 ]; then
      row storage bytes_per_drawer bytes 1 "$((PALACE_BYTES / DRAWERS))" "palace total bytes / drawers" "$P"
      row indexing index_bytes_per_drawer bytes 1 "$((INDEX_BYTES / DRAWERS))" "index.usearch.redb bytes / drawers" "$P"
    fi
  else
    row storage kg_stats_read bool 1 0 "palace stats failed or returned no JSON" "$P"
  fi

  # (e) dream stats: duration is indexing cost, recall scores are not correctness.
  DS="$PDIR/dream_stats.json"
  if [ -f "$DS" ] && jq -e . "$DS" >/dev/null 2>&1; then
    row indexing dream_duration_ms ms 1 "$(jq -c '.duration_ms // null' "$DS")" "last dream cycle; bimodal because embed passes are skipped when the drawer set is unchanged (#9391)" "$P"
    for m in recall_score_before recall_score_after; do
      row effectiveness "dream_$m" score 1 "$(jq -c ".$m // null" "$DS")" "mean top-3 cosine of 8 fixed queries; not a correctness measure" "$P"
    done
  fi
done < "$SCRATCH/top.tsv"

# --- envelope ------------------------------------------------------------------
LOAD_AFTER="$(load1)"
if [ "$SKIP_GATE" -eq 1 ]; then GATE=skipped
elif ge "$LOAD_AFTER" "$IDLE_MAX"; then GATE=failed
else GATE=passed; fi
[ "$GATE" != failed ] || echo "memory_perf_collect: idle gate failed (load $LOAD_AFTER after); discard this run" >&2

DOC="$(jq -s --arg version "$TOOL_VERSION" --arg commit "$COMMIT" --arg label "$LABEL" \
  --arg os "$HOST_OS" --arg cpu "$HOST_CPU" --argjson ram "$HOST_RAM_GB" \
  --arg date "$(date -u +%F)" --arg corpus "$CORPUS" --argjson before "$LOAD_BEFORE" \
  --argjson after "$LOAD_AFTER" --arg gate "$GATE" --argjson residency "$RESIDENCY" \
  --argjson top "$RANK" '
  {schema: 1, tool: "trusty-memory", version: $version, commit: $commit, label: $label,
   host: {id: $label, os: $os, cpu: $cpu, ram_gb: $ram}, date: $date, corpus: $corpus,
   load: {before: $before, after: $after}, idle_gate: $gate, residency: $residency,
   sampled_palaces: $top, results: .}' "$ROWS")"

printf '%s\n' "$DOC"
if [ -n "$OUT" ]; then printf '%s\n' "$DOC" > "$OUT"; fi
