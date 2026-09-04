#!/usr/bin/env bash
#
# measure-daemon-footprint_selftest.sh — fixtures for
# scripts/measure-daemon-footprint.sh (#6819).
#
# Why: the script it tests is the instrument every other child issue of epic
#   #6802 quotes its before/after numbers from, and its whole value rests on
#   two properties a live-daemon run cannot demonstrate. First, the Linux
#   branch: this machine is macOS, so `/proc/<pid>/status` parsing would ship
#   entirely unexercised without a fixture — and it is the branch CI would
#   run. Second, fail-closed: an operator who cannot tell "measured 0 MB"
#   from "could not measure" will publish the wrong number, so every path
#   that cannot produce a reading has to exit 2 with nothing on stdout, and
#   only a negative test proves that.
#
#   The `vmmap` case is pinned because the label it looks for, "Physical
#   footprint (peak)", was matched with `grep -E` at first — which reads the
#   parentheses as a group and silently matches the wrong line.
#
# What: five groups, all against fixtures — no daemon, no launchd, no network.
#   helpers    to_bytes / mb_from_bytes / human_bytes / canonical_daemon /
#              is_positive_int, including the units and the rejections
#   parsers    footprint's auxiliary block and category table, and vmmap's
#              summary line, against captured real output
#   proc       the Linux RssAnon / VmRSS ladder against four /proc fixtures
#   cli        argument parsing: --help, no args, bad option, bad daemon,
#              bad pid, a dead pid, a daemon named twice
#   closed     the fail-closed paths, each asserting exit 2 AND empty stdout
#
# Usage: bash scripts/measure-daemon-footprint_selftest.sh
# Exit: 0 when every case matches; 1 listing each mismatch.
#
# Portability: bash 3.2 (macOS system bash) and bash 5 (Linux CI), same as the
#   script under test.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}" || exit 1

SCRIPT="${REPO_ROOT}/scripts/measure-daemon-footprint.sh"
[ -r "$SCRIPT" ] || {
  echo "FAIL: ${SCRIPT} not found"
  exit 1
}

# Source the script in library mode: it defines every function and runs
# nothing, which is the seam that lets the parsers be tested directly.
# shellcheck source=scripts/measure-daemon-footprint.sh disable=SC1091
MEASURE_DAEMON_FOOTPRINT_LIB=1 . "$SCRIPT"

FIXTURES="$(mktemp -d "${TMPDIR:-/tmp}/measure-footprint-selftest.XXXXXX")"
trap 'rm -rf "${FIXTURES}"' EXIT

FAILURES=0
CASES=0

# eq <label> <expected> <actual>
eq() {
  local label="$1" want="$2" got="$3"
  CASES=$((CASES + 1))
  if [ "$want" = "$got" ]; then
    printf '  ok   %-56s -> %s\n' "$label" "$got"
  else
    FAILURES=$((FAILURES + 1))
    printf '  FAIL %-56s -> expected %s, got %s\n' "$label" "$want" "$got"
  fi
}

# rc <label> <expected-rc> <command...>
rc() {
  local label="$1" want="$2" got=0
  shift 2
  "$@" >/dev/null 2>&1 || got=$?
  eq "$label" "$want" "$got"
}

# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

cat >"${FIXTURES}/footprint.txt" <<'EOF'
======================================================================
trusty-search [42054]: 64-bit    Footprint: 9954533592 B (16384 bytes per page)
======================================================================

      Dirty         Clean   Reclaimable    Regions    Category
        ---           ---           ---        ---    ---
6342688768 B           0 B    99532800 B       6747    MALLOC_SMALL
2223816704 B           0 B           0 B        135    untagged (VM_ALLOCATE)
1230995456 B           0 B           0 B        322    MALLOC_LARGE
        0 B   151814144 B           0 B         54    mapped file
        0 B           0 B           0 B        157    __AUTH
        ---           ---           ---        ---    ---
9954500824 B   175489024 B    99549184 B      10327    TOTAL

Auxiliary data:
    phys_footprint: 9954533592 B
    phys_footprint_peak: 29108756384 B
EOF

# The default `formatted` output, to pin that the unit is parsed and not
# assumed to be bytes.
cat >"${FIXTURES}/footprint-formatted.txt" <<'EOF'
Auxiliary data:
    phys_footprint: 9493 MB
    phys_footprint_peak: 27 GB
EOF

cat >"${FIXTURES}/vmmap.txt" <<'EOF'
Process:         trusty-search [42054]
Physical footprint:         9.3G
Physical footprint (peak):  27.1G
ReadOnly portion of Libraries: Total=1.0G resident=379.9M
EOF

# RssAnon present — the primary reading on Linux, matching the choice
# core::memguard_enforce::anon_rss_mb_for_pid makes.
printf 'Name:\ttrusty-search\nState:\tS (sleeping)\nVmHWM:\t 9700000 kB\nVmRSS:\t 9500000 kB\nRssAnon:\t 9000000 kB\nRssFile:\t  480000 kB\nRssShmem:\t   20000 kB\n' \
  >"${FIXTURES}/status-full"

# A kernel or hardened container that exposes no RssAnon: must fall back to
# VmRSS, not fail.
printf 'Name:\ttrusty-search\nVmHWM:\t 9700000 kB\nVmRSS:\t 9500000 kB\n' \
  >"${FIXTURES}/status-no-anon"

# RssAnon reported as 0 — the exact shape that must NOT be published as a
# measurement. Falls through to VmRSS.
printf 'Name:\ttrusty-search\nVmRSS:\t 9500000 kB\nRssAnon:\t       0 kB\n' \
  >"${FIXTURES}/status-zero-anon"

# Neither field: unmeasurable, must exit 2.
printf 'Name:\ttrusty-search\nState:\tS (sleeping)\n' \
  >"${FIXTURES}/status-empty"

# ---------------------------------------------------------------------------
# 1. Helpers
# ---------------------------------------------------------------------------

echo "helpers:"
eq "to_bytes 1 B" 1 "$(to_bytes 1 B)"
eq "to_bytes 2 KB" 2048 "$(to_bytes 2 KB)"
eq "to_bytes 1 MB" 1048576 "$(to_bytes 1 MB)"
eq "to_bytes 27 GB" 28991029248 "$(to_bytes 27 GB)"
eq "to_bytes 9.3 G (fractional)" 9985798963 "$(to_bytes 9.3 G)"
eq "to_bytes lowercase unit" 1073741824 "$(to_bytes 1 g)"
rc "to_bytes rejects a junk unit" 1 to_bytes 1 furlongs
rc "to_bytes rejects a non-number" 1 to_bytes 12x MB
eq "mb_from_bytes truncates" 9493 "$(mb_from_bytes 9954533592)"
eq "human_bytes 1.5G" 1.5G "$(human_bytes 1639247872)"
eq "human_bytes bytes stay bytes" 512B "$(human_bytes 512)"
eq "canonical_daemon search" trusty-search "$(canonical_daemon search)"
eq "canonical_daemon TRUSTY-SEARCH" trusty-search "$(canonical_daemon TRUSTY-SEARCH)"
eq "canonical_daemon memory" trusty-memory "$(canonical_daemon memory)"
rc "canonical_daemon rejects an unknown name" 1 canonical_daemon postgres
eq "launchd_label search" com.trusty.search "$(launchd_label trusty-search)"
rc "is_positive_int rejects 0" 1 is_positive_int 0
rc "is_positive_int rejects empty" 1 is_positive_int ""
rc "is_positive_int rejects 12abc" 1 is_positive_int 12abc
rc "is_positive_int accepts 42054" 0 is_positive_int 42054

# ---------------------------------------------------------------------------
# 2. footprint / vmmap parsers
# ---------------------------------------------------------------------------

echo "parsers:"
eq "footprint phys_footprint (bytes)" 9954533592 \
  "$(parse_footprint_field "${FIXTURES}/footprint.txt" phys_footprint)"
eq "footprint phys_footprint_peak (bytes)" 29108756384 \
  "$(parse_footprint_field "${FIXTURES}/footprint.txt" phys_footprint_peak)"
# The `formatted` default prints MB/GB, so the unit has to be honoured. This
# also pins that a `phys_footprint` lookup is not satisfied by the
# `phys_footprint_peak` line sitting right under it.
eq "footprint formatted MB is converted" 9954131968 \
  "$(parse_footprint_field "${FIXTURES}/footprint-formatted.txt" phys_footprint)"
eq "footprint formatted GB peak is converted" 28991029248 \
  "$(parse_footprint_field "${FIXTURES}/footprint-formatted.txt" phys_footprint_peak)"
rc "footprint field absent -> failure" 1 \
  parse_footprint_field "${FIXTURES}/vmmap.txt" phys_footprint

eq "categories: biggest first" "6342688768	MALLOC_SMALL" \
  "$(parse_footprint_categories "${FIXTURES}/footprint.txt" | head -1)"
eq "categories: multi-word name kept whole" "2223816704	untagged (VM_ALLOCATE)" \
  "$(parse_footprint_categories "${FIXTURES}/footprint.txt" | sed -n 2p)"
eq "categories: TOTAL row dropped" 0 \
  "$(parse_footprint_categories "${FIXTURES}/footprint.txt" | grep -c TOTAL || true)"
eq "categories: zero-dirty rows dropped" 3 \
  "$(parse_footprint_categories "${FIXTURES}/footprint.txt" | grep -c . || true)"

eq "vmmap Physical footprint" 9985798963 \
  "$(parse_vmmap_footprint "${FIXTURES}/vmmap.txt" "Physical footprint")"
# The regression: "(peak)" read as an ERE group matched the non-peak line.
eq "vmmap Physical footprint (peak)" 29098403430 \
  "$(parse_vmmap_footprint "${FIXTURES}/vmmap.txt" "Physical footprint (peak)")"
rc "vmmap label absent -> failure" 1 \
  parse_vmmap_footprint "${FIXTURES}/footprint.txt" "Physical footprint"

# ---------------------------------------------------------------------------
# 3. Linux /proc/<pid>/status ladder
# ---------------------------------------------------------------------------

echo "proc:"
eq "RssAnon kB" 9000000 "$(parse_proc_status_kb "${FIXTURES}/status-full" RssAnon)"
eq "VmRSS kB" 9500000 "$(parse_proc_status_kb "${FIXTURES}/status-full" VmRSS)"
eq "RssFile kB" 480000 "$(parse_proc_status_kb "${FIXTURES}/status-full" RssFile)"
rc "RssAnon absent -> failure" 1 \
  parse_proc_status_kb "${FIXTURES}/status-no-anon" RssAnon
rc "RssAnon of 0 is not a reading" 1 \
  parse_proc_status_kb "${FIXTURES}/status-zero-anon" RssAnon
rc "empty status -> no VmRSS either" 1 \
  parse_proc_status_kb "${FIXTURES}/status-empty" VmRSS

# End to end in forced-Linux mode against each fixture. `--pid $$` is this
# shell, so the liveness check passes without a daemon.
run_linux() {
  MEASURE_FOOTPRINT_PLATFORM=linux \
    MEASURE_FOOTPRINT_PROC_STATUS="$1" \
    bash "$SCRIPT" --pid "$$" --json --no-health --no-disk 2>/dev/null
}

FULL_JSON="$(run_linux "${FIXTURES}/status-full")"
eq "linux e2e: bytes = RssAnon * 1024" 9216000000 \
  "$(printf '%s' "$FULL_JSON" | sed -n 's/.*"phys_footprint_bytes": \([0-9]*\).*/\1/p')"
eq "linux e2e: mb" 8789 \
  "$(printf '%s' "$FULL_JSON" | sed -n 's/.*"phys_footprint_mb": \([0-9]*\).*/\1/p')"
eq "linux e2e: source names the field used" '"source": "proc-status-rssanon",' \
  "$(printf '%s' "$FULL_JSON" | grep '"source"' | sed 's/^ *//')"
eq "linux e2e: peak from VmHWM" 9472 \
  "$(printf '%s' "$FULL_JSON" | sed -n 's/.*"phys_footprint_peak_mb": \([0-9]*\).*/\1/p')"
eq "linux e2e: breakdown lists RssAnon first" '"category": "RssAnon"' \
  "$(printf '%s' "$FULL_JSON" | grep -o '"category": "[A-Za-z]*"' | head -1)"

NOANON_JSON="$(run_linux "${FIXTURES}/status-no-anon")"
eq "linux e2e: falls back to VmRSS" '"source": "proc-status-vmrss",' \
  "$(printf '%s' "$NOANON_JSON" | grep '"source"' | sed 's/^ *//')"
eq "linux e2e: VmRSS bytes" 9728000000 \
  "$(printf '%s' "$NOANON_JSON" | sed -n 's/.*"phys_footprint_bytes": \([0-9]*\).*/\1/p')"

ZERO_JSON="$(run_linux "${FIXTURES}/status-zero-anon")"
eq "linux e2e: RssAnon 0 does not become the number" '"source": "proc-status-vmrss",' \
  "$(printf '%s' "$ZERO_JSON" | grep '"source"' | sed 's/^ *//')"

# ---------------------------------------------------------------------------
# 4. JSON key set
# ---------------------------------------------------------------------------

echo "json:"
if command -v jq >/dev/null 2>&1; then
  eq "valid JSON" ok "$(printf '%s' "$FULL_JSON" | jq -e . >/dev/null 2>&1 && echo ok || echo bad)"
  eq "top-level keys" \
    "schema daemon pid platform measured_at source phys_footprint_bytes phys_footprint_mb phys_footprint_peak_mb health data_dir units breakdown" \
    "$(printf '%s' "$FULL_JSON" | jq -r 'keys_unsorted | join(" ")')"
  eq "health keys" "rss_mb endpoint" \
    "$(printf '%s' "$FULL_JSON" | jq -r '.health | keys_unsorted | join(" ")')"
  eq "data_dir keys" "path size_kb size_human" \
    "$(printf '%s' "$FULL_JSON" | jq -r '.data_dir | keys_unsorted | join(" ")')"
  eq "units keys" "kind count resident chunks drawers" \
    "$(printf '%s' "$FULL_JSON" | jq -r '.units | keys_unsorted | join(" ")')"
  eq "breakdown entry keys" "category bytes mb" \
    "$(printf '%s' "$FULL_JSON" | jq -r '.breakdown[0] | keys_unsorted | join(" ")')"
  eq "schema value" "trusty-footprint/1" \
    "$(printf '%s' "$FULL_JSON" | jq -r '.schema')"
  # An absent reading is null, never 0 — the whole point of the fail-closed
  # contract carried into the machine-readable output.
  eq "absent health is null, not 0" null \
    "$(printf '%s' "$FULL_JSON" | jq -r '.health.rss_mb')"
  eq "absent unit count is null, not 0" null \
    "$(printf '%s' "$FULL_JSON" | jq -r '.units.count')"
else
  echo "  SKIP jq not installed — key-set assertions need it"
fi

# ---------------------------------------------------------------------------
# 5. CLI parsing and the fail-closed paths
# ---------------------------------------------------------------------------

echo "cli:"
rc "--help exits 0" 0 bash "$SCRIPT" --help
eq "--help prints the usage block" 1 \
  "$(bash "$SCRIPT" --help 2>/dev/null | grep -c '^Usage:' || true)"
rc "no arguments exits 2" 2 bash "$SCRIPT"
rc "unknown option exits 2" 2 bash "$SCRIPT" --nope
rc "unknown daemon exits 2" 2 bash "$SCRIPT" postgres
rc "daemon named twice exits 2" 2 bash "$SCRIPT" search memory
rc "--daemon with no value exits 2" 2 bash "$SCRIPT" --daemon
rc "--pid with no value exits 2" 2 bash "$SCRIPT" --pid
rc "--pid abc exits 2" 2 bash "$SCRIPT" --pid abc
rc "--pid 0 exits 2" 2 bash "$SCRIPT" --pid 0
# Above PID_MAX on macOS and Linux alike, so it can never be alive.
rc "--pid of a dead process exits 2" 2 bash "$SCRIPT" --pid 4194304

echo "fail-closed:"
# stdout must be EMPTY on every refusal: a partial table or a bare 0 would be
# read as a measurement.
closed() {
  local label="$1"
  shift
  local out rc_got=0
  out="$("$@" 2>/dev/null)" || rc_got=$?
  eq "${label}: exit 2" 2 "$rc_got"
  eq "${label}: stdout empty" "" "$out"
}

closed "unmeasurable /proc" env \
  MEASURE_FOOTPRINT_PLATFORM=linux \
  MEASURE_FOOTPRINT_PROC_STATUS="${FIXTURES}/status-empty" \
  bash "$SCRIPT" --pid "$$" --json --no-health --no-disk
closed "missing /proc file" env \
  MEASURE_FOOTPRINT_PLATFORM=linux \
  MEASURE_FOOTPRINT_PROC_STATUS="${FIXTURES}/does-not-exist" \
  bash "$SCRIPT" --pid "$$" --json --no-health --no-disk
closed "unsupported platform" env \
  MEASURE_FOOTPRINT_PLATFORM=plan9 \
  bash "$SCRIPT" --pid "$$" --json
closed "dead pid" bash "$SCRIPT" --pid 4194304 --json

# ---------------------------------------------------------------------------

echo
if [ "$FAILURES" -eq 0 ]; then
  echo "measure-daemon-footprint selftest: ${CASES} cases, all passed"
  exit 0
fi
echo "measure-daemon-footprint selftest: ${FAILURES} of ${CASES} cases FAILED"
exit 1
