#!/usr/bin/env bash
#
# ci-apt-install.sh — install apt packages on a CI runner with a bounded,
# observable retry (issue #5999).
#
# Why: every "Install system dependencies" / "Install Tauri system
#   dependencies" step in ci.yml ran `sudo apt-get update -qq` with no retry and
#   no timeout, under the job's `timeout-minutes: 30`. A stalled apt mirror
#   produces NO output, so the step consumed the whole 30-minute budget and the
#   job was killed before cargo ran even once. Observed twice on 2026-08-18:
#   three required jobs on a PR at ~01:16 UTC, and `trusty-agents-ui clippy` +
#   `trusty-code-gui clippy` on PR #5992 at 30m21s/30m22s (run 32158580446).
#   The PR reads red on code that is fine, and the whole run has to be redone.
#
# What: runs `apt-get update` then `apt-get install`, each attempt wrapped in
#   `timeout` and each phase retried up to ATTEMPTS times, with the whole thing
#   capped by a total budget. A stall now dies at the per-attempt timeout with
#   an `::error::` line naming the phase, the attempt, and the elapsed seconds,
#   instead of stalling silently until the job is cancelled.
#
#   Defaults (each overridable by environment variable, which is also how the
#   self-test drives every branch without waiting on a real mirror):
#
#     CI_APT_ATTEMPTS=3          attempts per phase
#     CI_APT_UPDATE_TIMEOUT_S=120  per-attempt ceiling for `apt-get update`
#     CI_APT_INSTALL_TIMEOUT_S=420 per-attempt ceiling for `apt-get install`
#     CI_APT_RETRY_DELAY_S=5     backoff unit; attempt N waits N*this seconds
#     CI_APT_TOTAL_BUDGET_S=600  hard ceiling across both phases
#     CI_APT_DPKG_REPAIR_TIMEOUT_S=120  ceiling for the between-attempt repair
#
#   The total budget is the number that answers the issue: worst case is ten
#   minutes, not thirty, and the ten minutes are spent visibly. Each attempt's
#   ceiling is clamped to what is left of that budget, so the budget is a real
#   ceiling rather than a value only sampled between attempts (#6064).
#
#   Between attempts the script runs `dpkg --configure -a` (#6064). Killing
#   `apt-get install` at its ceiling mid-unpack leaves dpkg's database
#   interrupted, and every later attempt then dies in seconds with "dpkg was
#   interrupted, you must manually run 'sudo dpkg --configure -a'" — so one
#   stall used to consume all three attempts. On a healthy system the repair is
#   a fast no-op.
#
#   `timeout` is a GNU coreutils tool. It is present on the hosted Linux
#   runners this script is written for; when it is absent (a bare macOS shell,
#   for instance) the command runs unwrapped and a warning says so, because
#   refusing to install is worse than installing without a per-attempt ceiling.
#   The retry and the total budget still apply.
#
# Usage: bash scripts/ci-apt-install.sh <package> [<package> ...]
#
# Exit: 0 when the packages are installed. 1 when a phase exhausted its
#   attempts or the total budget; the last apt exit code is reported (124 is
#   `timeout`'s "the command was still running", i.e. the stall this exists
#   for). 2 on usage error.
#
# Scope: `.github/workflows/ci.yml` routes every apt step through this. The
#   release and pre-publish workflows still inline the raw two-liner; they can
#   adopt this the next time one of them is touched.
#
# Test: scripts/check-ci-helpers-selftest.sh (`ci-apt-install:` cases) drives
#   the succeeds-first-try, succeeds-after-a-retry, exhausts-attempts,
#   stalls-then-times-out, and stall-breaks-dpkg-then-recovers branches against
#   a stubbed `apt-get` and `dpkg` on PATH.

set -uo pipefail

if [ "$#" -eq 0 ]; then
  echo "usage: bash scripts/ci-apt-install.sh <package> [<package> ...]" >&2
  exit 2
fi

ATTEMPTS="${CI_APT_ATTEMPTS:-3}"
UPDATE_TIMEOUT_S="${CI_APT_UPDATE_TIMEOUT_S:-120}"
INSTALL_TIMEOUT_S="${CI_APT_INSTALL_TIMEOUT_S:-420}"
RETRY_DELAY_S="${CI_APT_RETRY_DELAY_S:-5}"
TOTAL_BUDGET_S="${CI_APT_TOTAL_BUDGET_S:-600}"
DPKG_REPAIR_TIMEOUT_S="${CI_APT_DPKG_REPAIR_TIMEOUT_S:-120}"

STARTED_AT="$(date +%s)"

elapsed() { echo "$(( $(date +%s) - STARTED_AT ))"; }

TIMEOUT_BIN="$(command -v timeout || command -v gtimeout || true)"
if [ -z "$TIMEOUT_BIN" ]; then
  echo "ci-apt-install: no 'timeout' on PATH — running apt unwrapped; the retry and total budget still apply." >&2
fi

SUDO=""
if [ "$(id -u)" -ne 0 ] && command -v sudo >/dev/null 2>&1; then
  SUDO="sudo"
fi

# repair_dpkg — clear a dpkg database left "interrupted" by a killed apt-get.
#
# #6064: a `timeout`-killed `apt-get install` poisons every later attempt, which
# then exits 100 in seconds telling the reader to run this by hand. Nothing is
# gated on the failure text — on a healthy system the command is a fast no-op,
# and its own failure is reported but never fatal, because the retry it precedes
# is still worth making.
repair_dpkg() {
  command -v dpkg >/dev/null 2>&1 || return 0

  echo "ci-apt-install: running 'dpkg --configure -a' to clear any interrupted dpkg state before the next attempt." >&2
  local rc=0
  if [ -n "$TIMEOUT_BIN" ]; then
    "$TIMEOUT_BIN" "$DPKG_REPAIR_TIMEOUT_S" ${SUDO:+"$SUDO"} dpkg --configure -a || rc=$?
  else
    ${SUDO:+"$SUDO"} dpkg --configure -a || rc=$?
  fi
  [ "$rc" -eq 0 ] ||
    echo "ci-apt-install: 'dpkg --configure -a' exited ${rc}; retrying the phase regardless." >&2
  return 0
}

# run_phase <label> <per-attempt-timeout-s> <apt-get args...>
#
# Returns 0 on the first attempt that succeeds; 1 once the attempts or the total
# budget are exhausted, having printed a GitHub `::error::` annotation naming
# which of the two ran out.
run_phase() {
  local label="$1" per_attempt="$2"
  shift 2

  local attempt=1 rc=0 spent delay=0 ceiling="$per_attempt"
  while [ "$attempt" -le "$ATTEMPTS" ]; do
    spent="$(elapsed)"
    if [ "$spent" -ge "$TOTAL_BUDGET_S" ]; then
      echo "::error::ci-apt-install: ${label} abandoned after ${spent}s — over the ${TOTAL_BUDGET_S}s total budget. The apt mirror is not responding; re-run the job."
      return 1
    fi

    # #6064: clamp the attempt to what is left of the total budget, so a late
    # attempt cannot run the job past a ceiling only sampled between attempts.
    ceiling="$per_attempt"
    if [ "$ceiling" -gt $(( TOTAL_BUDGET_S - spent )) ]; then
      ceiling=$(( TOTAL_BUDGET_S - spent ))
    fi

    echo "ci-apt-install: ${label}, attempt ${attempt}/${ATTEMPTS} (${spent}s elapsed, ${ceiling}s ceiling)" >&2
    rc=0
    if [ -n "$TIMEOUT_BIN" ]; then
      "$TIMEOUT_BIN" "$ceiling" ${SUDO:+"$SUDO"} "$@" || rc=$?
    else
      ${SUDO:+"$SUDO"} "$@" || rc=$?
    fi

    if [ "$rc" -eq 0 ]; then
      echo "ci-apt-install: ${label} OK after ${attempt} attempt(s), $(elapsed)s elapsed" >&2
      return 0
    fi

    if [ "$rc" -eq 124 ]; then
      echo "ci-apt-install: ${label} attempt ${attempt} STALLED — killed at the ${ceiling}s ceiling." >&2
    else
      echo "ci-apt-install: ${label} attempt ${attempt} failed (exit ${rc})." >&2
    fi

    delay=$(( RETRY_DELAY_S * attempt ))
    attempt=$((attempt + 1))
    if [ "$attempt" -le "$ATTEMPTS" ]; then
      # #6064: repair before the retry, not after the phase — the poisoned
      # attempts are the ones inside this loop.
      repair_dpkg
      [ "$delay" -gt 0 ] && sleep "$delay"
    fi
  done

  echo "::error::ci-apt-install: ${label} failed ${ATTEMPTS} time(s), last exit ${rc}, $(elapsed)s elapsed. Exit 124 means the mirror stalled and the attempt was killed at its ${ceiling}s ceiling."
  return 1
}

run_phase "apt-get update" "$UPDATE_TIMEOUT_S" apt-get update -qq || exit 1
run_phase "apt-get install" "$INSTALL_TIMEOUT_S" \
  apt-get install -y --no-install-recommends "$@" || exit 1

echo "ci-apt-install: installed $* in $(elapsed)s" >&2
