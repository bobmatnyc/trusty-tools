#!/usr/bin/env bash
#
# ci-free-disk-space.sh — reclaim runner disk only when the runner needs it (#5325).
#
# Why: #4402 measured that `docker image prune` cost ~125s to reclaim ~2GB on a
#   runner that already had 87GB free, and gated it behind a headroom check. The
#   `sudo rm -rf` of unused SDKs sitting directly above that gate was left
#   ungated, and it is the more expensive half: 147s in the `test` job of run
#   31410011416 (16:39:08 -> 16:41:35) to take 87G available up to 115G, against
#   the ~27G peak #4402 measured for that job. The same step runs in four jobs.
#   Deleting ~29GB of files nobody reads is not free just because it is not a
#   prune.
#
#   Gated rather than deleted, for the same reason #4402 gave: #3668 was a real
#   disk-exhaustion incident, so the reclamation stays available as a safety net
#   and fires on its own if a future runner image or a heavier suite erodes the
#   margin.
#
# What: a three-tier decision on one measured number, the available GiB on `/`.
#
#     avail >= PURGE_FLOOR (45G)   -> skip both. Enough headroom for the job's
#                                     peak with ~1.7x margin.
#     avail <  PURGE_FLOOR         -> purge unused SDKs, then re-measure.
#     re-measured < PRUNE_FLOOR    -> also run `docker image prune` (#4402).
#
#   Every branch prints a `decision=` line, which is the contract the selftest
#   asserts on and the line to read in a job log when disk behaviour surprises
#   you. Exit status is 0 on every path: this step reclaims disk opportunistically
#   and must never be the reason a build fails.
#
# Usage: bash scripts/ci-free-disk-space.sh [--dry-run]
# Test: scripts/check-ci-helpers-selftest.sh ("ci-free-disk-space" section)

set -uo pipefail

# Thresholds in GiB. Overridable so the selftest can drive every branch without
# a runner that actually has the corresponding amount of disk.
PURGE_FLOOR_GB="${CI_DISK_PURGE_FLOOR_GB:-45}"
PRUNE_FLOOR_GB="${CI_DISK_PRUNE_FLOOR_GB:-25}"

DRY_RUN=0
if [ "${1:-}" = "--dry-run" ]; then
  DRY_RUN=1
fi

# Available GiB on /. CI_DISK_AVAIL_GB is the selftest's injection point; unset
# in CI, where the real `df` is what decides.
#
# The `^[0-9]+$` guard is the whole safety argument for this function, and it is
# deliberate rather than emergent. Without it an unparseable `df` (missing
# --output on a non-GNU coreutils, a header-only read, a mount that vanished)
# yields an empty string, bash coerces it to 0 inside $(( )), 0 is below both
# floors, and the script reclaims — which is the safe direction, but only by
# accident of two coercions nothing tests. Stating it makes the fallback a
# decision: an unreadable disk reads as FULL, so the reclamation still fires.
# Test: `scripts/check-ci-helpers-selftest.sh`, "df output is not a number".
measure_avail_gb() {
  if [ -n "${CI_DISK_AVAIL_GB:-}" ]; then
    echo "${CI_DISK_AVAIL_GB}"
    return
  fi
  local kb
  kb=$(df -k --output=avail / 2>/dev/null | tail -1 | tr -d ' ')
  if ! [[ "${kb}" =~ ^[0-9]+$ ]]; then
    echo "::warning::could not read available disk from df (got '${kb}') — treating as 0G, which reclaims" >&2
    kb=0
  fi
  echo $(( kb / 1024 / 1024 ))
}

# Available GiB after the SDK purge. Separate injection point so the selftest can
# express "the purge ran and still did not free enough".
measure_avail_after_gb() {
  if [ -n "${CI_DISK_AVAIL_AFTER_GB:-}" ]; then
    echo "${CI_DISK_AVAIL_AFTER_GB}"
    return
  fi
  measure_avail_gb
}

purge_unused_sdks() {
  [ "${DRY_RUN}" -eq 1 ] && return 0
  # None of these are touched by a pure-Rust build or by this workspace's tests.
  sudo rm -rf /usr/share/dotnet /opt/ghc /usr/local/lib/android \
    /usr/local/share/boost /usr/local/share/powershell \
    /usr/share/swift "${AGENT_TOOLSDIRECTORY:-/opt/hostedtoolcache}" \
    /usr/local/share/vcpkg /usr/lib/jvm /usr/share/miniconda /opt/az || true
  sudo apt-get clean || true
  sudo rm -rf /var/lib/apt/lists/* || true
}

prune_docker_images() {
  [ "${DRY_RUN}" -eq 1 ] && return 0
  sudo docker image prune --all --force || true
}

[ "${DRY_RUN}" -eq 0 ] && { echo "Before:"; df -h /; }

avail=$(measure_avail_gb)
echo "Available on /: ${avail}G (purge floor ${PURGE_FLOOR_GB}G, prune floor ${PRUNE_FLOOR_GB}G)"

if [ "${avail}" -ge "${PURGE_FLOOR_GB}" ]; then
  echo "decision=skip-all"
  echo "Skipping SDK purge and docker prune: ${avail}G free (>= ${PURGE_FLOOR_GB}G, #5325)"
  exit 0
fi

echo "::warning::only ${avail}G free (< ${PURGE_FLOOR_GB}G) — purging unused SDKs"
purge_unused_sdks

after=$(measure_avail_after_gb)
echo "Avail after SDK purge: ${after}G"

if [ "${after}" -lt "${PRUNE_FLOOR_GB}" ]; then
  echo "decision=purge+prune"
  echo "::warning::still only ${after}G free (< ${PRUNE_FLOOR_GB}G) — running docker image prune"
  prune_docker_images
else
  echo "decision=purge"
  echo "Skipping docker image prune: ${after}G free (>= ${PRUNE_FLOOR_GB}G, #4402)"
fi

[ "${DRY_RUN}" -eq 0 ] && { echo "After:"; df -h /; }

exit 0
