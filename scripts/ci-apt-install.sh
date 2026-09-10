#!/usr/bin/env bash
#
# ci-apt-install.sh — install apt packages on a CI runner with a bounded,
# observable retry (issue #5999), and report an apt INFRASTRUCTURE failure as
# one classifiable event rather than N unrelated code reds (issue #7288).
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
#   #7288 is the second half of the same complaint. The retry bounded the cost
#   but said nothing about the CAUSE, so a single Chrome-mirror `Hash Sum
#   mismatch` killed 8 jobs on PR #7257 inside a 24-second window and each one
#   read as its own code red. `notify-main-failure` and `red-main-notify.yml`
#   compose their verdict from job conclusions alone (see
#   scripts/classify-ci-results.sh), and GitHub gives them nothing but
#   `failure` — so eight jobs that died on one mirror get reported, and
#   re-diagnosed by a human, eight times.
#
# What: runs `apt-get update` then `apt-get install`, each attempt wrapped in
#   `timeout` and each phase retried up to ATTEMPTS times, with the whole thing
#   capped by a total budget. A stall now dies at the per-attempt timeout with
#   an `::error::` line naming the phase, the attempt, and the elapsed seconds,
#   instead of stalling silently until the job is cancelled.
#
#   Each attempt's output is also matched against the signatures of an apt
#   infrastructure failure — hash-sum mismatch, a mirror answering 4xx/5xx, DNS
#   that will not resolve, a connection that will not open, and the stall
#   itself. When a phase exhausts its attempts on one of those, the script
#   exits 75 (not 1) and prints ONE annotation carrying two greppable tokens:
#
#     ::error::apt-infra: <reason> — INFRA-FAIL: apt — <details>
#
#   `apt-infra:` is what a log scan matches; `INFRA-FAIL: apt` is the token
#   named in #7288's closure condition. Both are on the same line on purpose,
#   so a consumer may grep for either. When `$GITHUB_OUTPUT` is set the same
#   verdict is written there as `apt_infra=true` / `apt_infra_reason=<reason>`,
#   which is the machine channel a job needs to lift the fact into a job output
#   and hand `classify-ci-results.sh` one infra event instead of N reds. Wiring
#   that through ci.yml's jobs is not done here.
#
#   A failure with NO infra signature — `Unable to locate package`, a held
#   dependency, a bad package name — still exits 1 under the original
#   `::error::ci-apt-install:` line. That is the case the distinction exists to
#   protect: a genuinely broken step must not be waved through as "the mirror".
#
#   A `hash-mismatch` reason also purges CI_APT_LISTS_DIR before the retry.
#   Retrying `apt-get update` against the stale index that mismatched
#   reproduces the same mismatch every time, so without the purge the two
#   remaining attempts are spent proving it.
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
#     CI_APT_LISTS_DIR=/var/lib/apt/lists  purged after a hash mismatch
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
# Exit: 0 when the packages are installed. 75 when a phase exhausted its
#   attempts on an apt INFRASTRUCTURE failure (the `apt-infra:` annotation says
#   which); the job is red but the commit is unjudged. 1 when a phase exhausted
#   its attempts or the total budget on anything else; the last apt exit code is
#   reported. 2 on usage error.
#
# Scope: `.github/workflows/ci.yml` routes every apt step through this. The
#   release and pre-publish workflows still inline the raw two-liner; they can
#   adopt this the next time one of them is touched.
#
# Test: scripts/check-ci-helpers-selftest.sh (`ci-apt-install:` cases) drives
#   the succeeds-first-try, succeeds-after-a-retry, exhausts-attempts,
#   stalls-then-times-out, and stall-breaks-dpkg-then-recovers branches against
#   a stubbed `apt-get` and `dpkg` on PATH, plus every #7288 classification
#   arm — each infra signature, the non-infra failure that must NOT be
#   classified as one, the lists purge, and the `$GITHUB_OUTPUT` rows.

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
LISTS_DIR="${CI_APT_LISTS_DIR:-/var/lib/apt/lists}"

# Exit code for "the mirror failed, not this commit". 75 is EX_TEMPFAIL, whose
# sysexits.h meaning — a temporary failure, the operation should be retried —
# is exactly the claim being made.
INFRA_EXIT=75

STARTED_AT="$(date +%s)"

elapsed() { echo "$(( $(date +%s) - STARTED_AT ))"; }

# One attempt's combined output, re-read by apt_infra_reason. Recreated per
# attempt; removed on exit however the script leaves.
ATTEMPT_LOG="$(mktemp "${TMPDIR:-/tmp}/ci-apt-attempt.XXXXXX")"
trap 'rm -f "${ATTEMPT_LOG}"' EXIT

TIMEOUT_BIN="$(command -v timeout || command -v gtimeout || true)"
if [ -z "$TIMEOUT_BIN" ]; then
  echo "ci-apt-install: no 'timeout' on PATH — running apt unwrapped; the retry and total budget still apply." >&2
fi

SUDO=""
if [ "$(id -u)" -ne 0 ] && command -v sudo >/dev/null 2>&1; then
  SUDO="sudo"
fi

# apt_infra_reason <exit-code> — echo a one-word reason when this attempt failed
# the way an apt MIRROR fails, and nothing when it failed the way a package
# request fails (#7288).
#
# The reasons are the four signatures #7288 names plus the stall the wrapper
# already detects. Order matters only in that the first match wins; each arm is
# a distinct remediation, so the label is worth keeping distinct.
#
# The HTTP arm requires BOTH a fetch error and a 4xx/5xx status, because a bare
# three-digit number appears in package names and versions. Fail closed the
# other way here: an unmatched failure is reported as a code failure, so a real
# break is never laundered into "the mirror was flaky".
apt_infra_reason() {
  local rc="$1"

  # A killed attempt is a mirror that accepted the connection and then stopped
  # answering. There is usually no output at all to match on.
  if [ "$rc" -eq 124 ]; then
    echo "stall"
    return 0
  fi

  [ -s "$ATTEMPT_LOG" ] || return 0

  if grep -qiE 'hash sum mismatch|file has unexpected size' "$ATTEMPT_LOG"; then
    echo "hash-mismatch"
  elif grep -qiE 'temporary failure resolving|could not resolve' "$ATTEMPT_LOG"; then
    echo "dns"
  elif grep -qiE 'could not connect|unable to connect|connection (failed|refused|reset|timed out)' "$ATTEMPT_LOG"; then
    echo "connect"
  elif grep -qiE 'err:|failed to fetch|some index files failed to download' "$ATTEMPT_LOG"; then
    if grep -qE '(^|[^0-9])(4[0-9]{2}|5[0-9]{2})([^0-9]|$)' "$ATTEMPT_LOG"; then
      echo "mirror-http"
    else
      echo "mirror-fetch"
    fi
  fi
}

# clear_apt_lists — drop the cached package indexes before a hash-mismatch retry.
#
# #7288: a mismatch is between the Release file apt has and the index the mirror
# served, and apt keeps serving itself the cached copy, so every retry
# reproduces it. Deleting the lists forces a clean fetch. Never fatal — the
# retry is still worth making — and `:?` refuses to expand an empty variable
# into `rm -rf /*`.
clear_apt_lists() {
  [ -d "$LISTS_DIR" ] || return 0

  echo "ci-apt-install: purging ${LISTS_DIR} so the retry re-fetches the indexes instead of re-reading the mismatched ones." >&2
  ${SUDO:+"$SUDO"} rm -rf -- "${LISTS_DIR:?}"/* 2>/dev/null
  return 0
}

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

# report_infra <label> <reason> <attempts-made> <last-exit> — the single
# annotation every consumer greps, plus the same verdict on the $GITHUB_OUTPUT
# channel.
report_infra() {
  local label="$1" reason="$2" made="$3" rc="$4"

  echo "::error::apt-infra: ${reason} — INFRA-FAIL: apt — ${label} failed ${made} time(s) over $(elapsed)s, last exit ${rc}. The apt mirror failed, not this commit; every job installing packages in this window fails the same way. Re-run the jobs."

  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    echo "apt_infra=true" >>"${GITHUB_OUTPUT}"
    echo "apt_infra_reason=${reason}" >>"${GITHUB_OUTPUT}"
  fi
}

# run_phase <label> <per-attempt-timeout-s> <apt-get args...>
#
# Returns 0 on the first attempt that succeeds; 75 once the attempts are
# exhausted on an apt infrastructure failure; 1 once the attempts or the total
# budget are exhausted on anything else. Either failure prints a GitHub
# `::error::` annotation naming what ran out.
run_phase() {
  local label="$1" per_attempt="$2"
  shift 2

  local attempt=1 rc=0 spent delay=0 ceiling="$per_attempt" reason="" last_reason=""
  while [ "$attempt" -le "$ATTEMPTS" ]; do
    spent="$(elapsed)"
    if [ "$spent" -ge "$TOTAL_BUDGET_S" ]; then
      # #7288: an earlier attempt that already showed a mirror signature makes
      # this the same infra event, reported once, rather than a second verdict.
      if [ -n "$last_reason" ]; then
        report_infra "$label" "$last_reason" "$(( attempt - 1 ))" "$rc"
        return "$INFRA_EXIT"
      fi
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
    # #7288: the attempt's output is teed so it stays visible AND can be matched
    # for an infrastructure signature. `pipefail` (set at the top) is what keeps
    # apt's exit code rather than tee's.
    : >"${ATTEMPT_LOG}"
    if [ -n "$TIMEOUT_BIN" ]; then
      "$TIMEOUT_BIN" "$ceiling" ${SUDO:+"$SUDO"} "$@" 2>&1 | tee -a "${ATTEMPT_LOG}" >&2 || rc=$?
    else
      ${SUDO:+"$SUDO"} "$@" 2>&1 | tee -a "${ATTEMPT_LOG}" >&2 || rc=$?
    fi

    if [ "$rc" -eq 0 ]; then
      echo "ci-apt-install: ${label} OK after ${attempt} attempt(s), $(elapsed)s elapsed" >&2
      return 0
    fi

    reason="$(apt_infra_reason "$rc")"
    [ -z "$reason" ] || last_reason="$reason"

    if [ "$rc" -eq 124 ]; then
      echo "ci-apt-install: ${label} attempt ${attempt} STALLED — killed at the ${ceiling}s ceiling." >&2
    elif [ -n "$reason" ]; then
      echo "ci-apt-install: ${label} attempt ${attempt} failed (exit ${rc}) — apt infrastructure signature: ${reason}." >&2
    else
      echo "ci-apt-install: ${label} attempt ${attempt} failed (exit ${rc})." >&2
    fi

    delay=$(( RETRY_DELAY_S * attempt ))
    attempt=$((attempt + 1))
    if [ "$attempt" -le "$ATTEMPTS" ]; then
      # #6064: repair before the retry, not after the phase — the poisoned
      # attempts are the ones inside this loop.
      repair_dpkg
      [ "$reason" = "hash-mismatch" ] && clear_apt_lists
      [ "$delay" -gt 0 ] && sleep "$delay"
    fi
  done

  if [ -n "$last_reason" ]; then
    report_infra "$label" "$last_reason" "$ATTEMPTS" "$rc"
    return "$INFRA_EXIT"
  fi

  echo "::error::ci-apt-install: ${label} failed ${ATTEMPTS} time(s), last exit ${rc}, $(elapsed)s elapsed. Exit 124 means the mirror stalled and the attempt was killed at its ${ceiling}s ceiling."
  return 1
}

run_phase "apt-get update" "$UPDATE_TIMEOUT_S" apt-get update -qq || exit $?
run_phase "apt-get install" "$INSTALL_TIMEOUT_S" \
  apt-get install -y --no-install-recommends "$@" || exit $?

echo "ci-apt-install: installed $* in $(elapsed)s" >&2
