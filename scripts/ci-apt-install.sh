#!/usr/bin/env bash
#
# ci-apt-install.sh — install apt packages on a CI runner with a bounded,
# observable retry (issue #5999), report an apt INFRASTRUCTURE failure as one
# classifiable event rather than N unrelated code reds (issue #7288), and
# survive an Ubuntu mirror outage where it can (2026-11-11 outage).
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
#   On 2026-11-11 from ~14:40Z the runners could not resolve
#   `security.ubuntu.com`, `archive.ubuntu.com` or `azure.archive.ubuntu.com`
#   (runs 43037343719, 43037343501). The retry only re-asked the same dead
#   hosts, so every job turned red, including jobs whose packages the runner
#   image already ships.
#
# What: four routes, in order. The first that leaves the packages installed
#   wins.
#
#   1. Presence check. When `dpkg-query` reports every requested package as
#      `ok installed`, the script exits 0 and touches no network. It verifies
#      each package; it never assumes one. `build-essential` also counts as
#      present when every package it depends on (BUILD_ESSENTIAL_PARTS) is
#      installed, because the runner image ships those without the metapackage.
#      Only the packages still missing go to the later routes.
#   2. Cached-index install. One `apt-get install` against the package lists
#      the runner already has, with no `apt-get update` first. A mirror that
#      cannot serve its index can often still serve the .deb files, and when
#      the lists are empty this fails in under a second.
#   3. Primary mirror. `apt-get update` then `apt-get install`, each attempt
#      wrapped in `timeout` and each phase retried up to ATTEMPTS times. It runs
#      to PRIMARY_DEADLINE (TOTAL_BUDGET_S minus FALLBACK_RESERVE_S), which keeps
#      time for route 4. An update that exits 0 but logged a fetch or resolve
#      failure counts as a failed attempt. When the update failed on an infra
#      signature, the install still runs once, and that update verdict stands
#      if the install fails.
#   4. Fallback mirrors. Only after route 3 failed on an apt INFRASTRUCTURE
#      signature. The configured sources are copied with every
#      `*.archive.ubuntu.com/ubuntu` and `security.ubuntu.com/ubuntu` URI
#      replaced by a mirror outside `*.ubuntu.com` (CI_APT_FALLBACK_MIRRORS),
#      as is every `mirror+file:<list>` URI whose list names one of those,
#      and apt is pointed at the copy with `-o Dir::Etc::SourceList` /
#      `SourceParts`. The runner's own sources are not edited. Non-Ubuntu
#      sources (NVIDIA's CUDA repo, for example) are copied unchanged. The
#      archive signatures stay the same, so a plain-http mirror is safe.
#
#   Every failed attempt logs the hosts its output names (`failed host(s):`).
#   A stall prints no host, so the log names the configured source hosts
#   instead.
#
#   Each attempt's output is also matched against the signatures of an apt
#   infrastructure failure — hash-sum mismatch, a mirror answering 4xx/5xx, DNS
#   that will not resolve, a connection that will not open, and the stall
#   itself. When every route has failed and route 3 failed on one of those, the
#   script exits 75 (not 1) and prints ONE annotation carrying two greppable
#   tokens:
#
#     ::error::apt-infra: <reason> — INFRA-FAIL: apt — <details>
#
#   `apt-infra:` is what a log scan matches; `INFRA-FAIL: apt` is the token
#   named in #7288's closure condition. Both are on the same line on purpose,
#   so a consumer may grep for either. When `$GITHUB_OUTPUT` is set the same
#   verdict is written there as `apt_infra=true` / `apt_infra_reason=<reason>`.
#
#   A failure with NO infra signature — `Unable to locate package`, a held
#   dependency, a bad package name — still exits 1 under the original
#   `::error::ci-apt-install:` line, and no fallback mirror is tried. That is
#   the case the distinction exists to protect: a genuinely broken step must
#   not be waved through as "the mirror".
#
#   A `hash-mismatch` reason also purges CI_APT_LISTS_DIR before the retry.
#   Retrying `apt-get update` against the stale index that mismatched
#   reproduces the same mismatch every time.
#
#   Between attempts, and after a cached-index install that stalled, the script
#   runs `dpkg --configure -a` (#6064). Killing `apt-get install` at its ceiling
#   mid-unpack leaves dpkg's database interrupted, and every later attempt then
#   dies in seconds. On a healthy system the repair is a fast no-op.
#
#   Defaults (each overridable by environment variable, which is also how the
#   self-test drives every branch without waiting on a real mirror):
#
#     CI_APT_ATTEMPTS=3                attempts per primary phase
#     CI_APT_UPDATE_TIMEOUT_S=120      per-attempt ceiling for `apt-get update`
#     CI_APT_INSTALL_TIMEOUT_S=420     per-attempt ceiling for `apt-get install`
#     CI_APT_CACHED_INSTALL_TIMEOUT_S=90  ceiling for the route-2 install
#     CI_APT_RETRY_DELAY_S=5           backoff unit; attempt N waits N*this
#     CI_APT_TOTAL_BUDGET_S=600        hard ceiling across every route
#     CI_APT_FALLBACK_RESERVE_S=180    budget kept back for route 4
#     CI_APT_FALLBACK_MIRRORS="http://mirrors.edge.kernel.org/ubuntu http://mirrors.mit.edu/ubuntu"
#                                      space-separated; empty disables route 4
#     CI_APT_SOURCES_DIR=/etc/apt      where sources.list(.d) are read from
#     CI_APT_DPKG_REPAIR_TIMEOUT_S=120 ceiling for the between-attempt repair
#     CI_APT_LISTS_DIR=/var/lib/apt/lists  purged after a hash mismatch
#
#   The total budget stays ten minutes, the same as before route 4 existed.
#   Each attempt's ceiling is clamped to what is left of its route's deadline,
#   so the budget is a real ceiling (#6064).
#
#   `timeout` is a GNU coreutils tool. When it is absent the command runs
#   unwrapped and a warning says so; the retry and the budget still apply.
#
# Usage: bash scripts/ci-apt-install.sh <package> [<package> ...]
#
# Exit: 0 when the packages are installed (already present, or installed by a
#   route). 75 when every route failed and the primary mirror failed on an apt
#   INFRASTRUCTURE signature; the job is red but the commit is unjudged. 1 when
#   the primary mirror failed on anything else; the last apt exit code is
#   reported. 2 on usage error.
#
# Scope: every apt step in `.github/workflows/` and the remedy text in
#   scripts/check_semver.sh route through this.
#
# Test: scripts/check-ci-helpers-selftest.sh (`ci-apt-install:` cases) drives
#   every route against a stubbed `apt-get`, `dpkg` and `dpkg-query` on PATH:
#   the already-installed skip and its partial-presence negative, the
#   cached-index install, retry, exhaustion, stall, dpkg repair, every #7288
#   classification arm, the fallback-mirror rescue, and all-routes-fail.

set -uo pipefail

if [ "$#" -eq 0 ]; then
  echo "usage: bash scripts/ci-apt-install.sh <package> [<package> ...]" >&2
  exit 2
fi

ATTEMPTS="${CI_APT_ATTEMPTS:-3}"
UPDATE_TIMEOUT_S="${CI_APT_UPDATE_TIMEOUT_S:-120}"
INSTALL_TIMEOUT_S="${CI_APT_INSTALL_TIMEOUT_S:-420}"
CACHED_INSTALL_TIMEOUT_S="${CI_APT_CACHED_INSTALL_TIMEOUT_S:-90}"
RETRY_DELAY_S="${CI_APT_RETRY_DELAY_S:-5}"
TOTAL_BUDGET_S="${CI_APT_TOTAL_BUDGET_S:-600}"
FALLBACK_RESERVE_S="${CI_APT_FALLBACK_RESERVE_S:-180}"
# `-`, not `:-`: an explicitly empty value disables route 4.
FALLBACK_MIRRORS="${CI_APT_FALLBACK_MIRRORS-http://mirrors.edge.kernel.org/ubuntu http://mirrors.mit.edu/ubuntu}"
SOURCES_DIR="${CI_APT_SOURCES_DIR:-/etc/apt}"
DPKG_REPAIR_TIMEOUT_S="${CI_APT_DPKG_REPAIR_TIMEOUT_S:-120}"
LISTS_DIR="${CI_APT_LISTS_DIR:-/var/lib/apt/lists}"

# The packages `build-essential` depends on (Ubuntu 24.04). When all of them
# are installed, the metapackage adds nothing a build needs.
BUILD_ESSENTIAL_PARTS="libc6-dev gcc g++ make dpkg-dev"

# The Ubuntu archive URIs route 4 replaces. Anything else is left as it is.
UBUNTU_URI_RE='https?://([A-Za-z0-9-]+\.)*(archive|security)\.ubuntu\.com/ubuntu/?'

# Exit code for "the mirror failed, not this commit". 75 is EX_TEMPFAIL.
INFRA_EXIT=75

STARTED_AT="$(date +%s)"
DEADLINE_S="$TOTAL_BUDGET_S"
PRIMARY_DEADLINE_S=$(( TOTAL_BUDGET_S - FALLBACK_RESERVE_S ))
[ "$PRIMARY_DEADLINE_S" -gt 0 ] || PRIMARY_DEADLINE_S="$TOTAL_BUDGET_S"

elapsed() { echo "$(( $(date +%s) - STARTED_AT ))"; }

# One attempt's combined output, re-read by apt_infra_reason and failed_hosts,
# and a scratch directory for route 4's rewritten sources. Both removed on exit.
ATTEMPT_LOG="$(mktemp "${TMPDIR:-/tmp}/ci-apt-attempt.XXXXXX")"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/ci-apt-work.XXXXXX")"
trap 'rm -rf "${ATTEMPT_LOG}" "${WORK_DIR}"' EXIT

TIMEOUT_BIN="$(command -v timeout || command -v gtimeout || true)"
if [ -z "$TIMEOUT_BIN" ]; then
  echo "ci-apt-install: no 'timeout' on PATH — running apt unwrapped; the retry and total budget still apply." >&2
fi

SUDO=""
if [ "$(id -u)" -ne 0 ] && command -v sudo >/dev/null 2>&1; then
  SUDO="sudo"
fi

# The infra verdict of the last phase that ran out, reported once at the end so
# a fallback that recovers leaves no `::error::` behind. PHASE_FAIL_MSG is the
# non-infra annotation, printed only when the script gives up on it.
INFRA_REASON="" INFRA_LABEL="" INFRA_MADE=0 INFRA_RC=0 PHASE_FAIL_MSG=""
FAILED_HOSTS=""

# pkg_installed <pkg> — true only when dpkg records the package `ok installed`.
pkg_installed() {
  # shellcheck disable=SC2016 # ${Status} is a dpkg-query field, not a shell variable.
  dpkg-query -W -f='${Status}\n' "$1" 2>/dev/null | grep -q ' ok installed$'
}

# pkg_satisfied <pkg> — the package is installed, or it is build-essential and
# every package it depends on is installed.
pkg_satisfied() {
  local part
  pkg_installed "$1" && return 0
  [ "$1" = "build-essential" ] || return 1
  for part in $BUILD_ESSENTIAL_PARTS; do
    pkg_installed "$part" || return 1
  done
  return 0
}

# apt_infra_reason <exit-code> — a one-word reason when this attempt failed the
# way an apt MIRROR fails, nothing when it failed the way a package request
# fails (#7288). First match wins. The HTTP arm needs BOTH a fetch error and a
# 4xx/5xx status, because a bare three-digit number appears in versions. An
# unmatched failure is reported as a code failure, never as "the mirror".
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

# words_line — read words on stdin, print them unique, space-separated.
words_line() { sort -u | tr '\n' ' ' | sed 's/ *$//'; }

# failed_hosts — the hosts this attempt's output names as failing: the name DNS
# would not resolve, the host a connection would not open to, and the host of
# every `Err:` / `Failed to fetch` URI.
failed_hosts() {
  [ -s "$ATTEMPT_LOG" ] || return 0
  {
    grep -oE "resolving '[^']+'" "$ATTEMPT_LOG" | sed -E "s/^resolving '([^']+)'$/\1/"
    grep -oiE 'connect to [A-Za-z0-9.-]+' "$ATTEMPT_LOG" | sed -E 's/^[Cc]onnect to //'
    grep -E '^(Err|Ign|E|W):|[Ff]ailed to fetch' "$ATTEMPT_LOG" |
      grep -oE 'https?://[^/ ]+' | sed -E 's#^https?://##; s/:[0-9]+$//'
  } | words_line
}

# source_hosts <sources-dir> — every host the apt sources there point at, for a
# stall, whose output names none.
source_hosts() {
  local dir="$1"
  {
    cat "${dir}/sources.list" "${dir}/sources.list.d/"*.list "${dir}/sources.list.d/"*.sources 2>/dev/null |
      grep -v '^[[:space:]]*#' | grep -oE 'https?://[^/ ]+' | sed -E 's#^https?://##'
  } | words_line
}

# note_hosts <label> <attempt> <rc> <sources-dir> — log the hosts this failed
# attempt names, and add them to FAILED_HOSTS for the final annotation.
note_hosts() {
  local hosts
  hosts="$(failed_hosts)"
  if [ -n "$hosts" ]; then
    echo "ci-apt-install: $1 attempt $2 failed host(s): ${hosts}" >&2
    FAILED_HOSTS="$(printf '%s %s' "$FAILED_HOSTS" "$hosts" | tr ' ' '\n' | grep -v '^$' | words_line)"
  elif [ "$3" -eq 124 ]; then
    echo "ci-apt-install: $1 attempt $2 named no host; the sources it was reading point at: $(source_hosts "$4")" >&2
  fi
}

# clear_apt_lists — drop the cached package indexes before a hash-mismatch retry
# (#7288). Never fatal; `:?` refuses to expand an empty variable into `rm -rf /*`.
clear_apt_lists() {
  [ -d "$LISTS_DIR" ] || return 0

  echo "ci-apt-install: purging ${LISTS_DIR} so the retry re-fetches the indexes instead of re-reading the mismatched ones." >&2
  ${SUDO:+"$SUDO"} rm -rf -- "${LISTS_DIR:?}"/* 2>/dev/null
  return 0
}

# repair_dpkg — clear a dpkg database left "interrupted" by a killed apt-get
# (#6064). A fast no-op on a healthy system; its own failure is never fatal.
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

# report_infra <extra-detail> — the single annotation every consumer greps, plus
# the same verdict on the $GITHUB_OUTPUT channel.
report_infra() {
  local hosts="${FAILED_HOSTS:-none named}"
  echo "::error::apt-infra: ${INFRA_REASON} — INFRA-FAIL: apt — ${INFRA_LABEL} failed ${INFRA_MADE} time(s), last exit ${INFRA_RC}, $(elapsed)s elapsed; failed host(s): ${hosts}; $1 The apt mirror failed, not this commit; every job installing packages in this window fails the same way. Re-run the jobs."

  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    echo "apt_infra=true" >>"${GITHUB_OUTPUT}"
    echo "apt_infra_reason=${INFRA_REASON}" >>"${GITHUB_OUTPUT}"
  fi
}

# clamp_ceiling <per-attempt-s> — the ceiling, cut to what is left before
# DEADLINE_S (#6064).
clamp_ceiling() {
  local left=$(( DEADLINE_S - $(elapsed) ))
  if [ "$1" -gt "$left" ]; then echo "$left"; else echo "$1"; fi
}

# run_attempt <ceiling> <command...> — one attempt, teed so it stays visible and
# can be matched (#7288). `pipefail` keeps apt's exit code rather than tee's.
run_attempt() {
  local ceiling="$1" rc=0
  shift
  : >"${ATTEMPT_LOG}"
  if [ -n "$TIMEOUT_BIN" ]; then
    "$TIMEOUT_BIN" "$ceiling" ${SUDO:+"$SUDO"} "$@" 2>&1 | tee -a "${ATTEMPT_LOG}" >&2 || rc=$?
  else
    ${SUDO:+"$SUDO"} "$@" 2>&1 | tee -a "${ATTEMPT_LOG}" >&2 || rc=$?
  fi
  return "$rc"
}

# run_phase <label> <per-attempt-s> <attempts> <sources-dir> <apt-get args...>
#
# Returns 0 on the first attempt that succeeds; 75 once the attempts or the
# deadline run out after an apt infrastructure failure, with the INFRA_*
# globals set; 1 once they run out on anything else, with PHASE_FAIL_MSG set.
# It prints no `::error::` itself — the caller decides whether a later route
# makes the failure moot.
run_phase() {
  local label="$1" per_attempt="$2" attempts="$3" srcdir="$4"
  shift 4

  local attempt=1 rc=0 spent delay=0 ceiling="$per_attempt" reason="" last_reason=""
  while [ "$attempt" -le "$attempts" ]; do
    spent="$(elapsed)"
    if [ "$spent" -ge "$DEADLINE_S" ]; then
      # #7288: an earlier attempt that already showed a mirror signature makes
      # this the same infra event, reported once, rather than a second verdict.
      if [ -n "$last_reason" ]; then
        INFRA_REASON="$last_reason" INFRA_LABEL="$label" INFRA_MADE=$(( attempt - 1 )) INFRA_RC="$rc"
        return "$INFRA_EXIT"
      fi
      PHASE_FAIL_MSG="::error::ci-apt-install: ${label} abandoned after ${spent}s — over its ${DEADLINE_S}s budget. The apt mirror is not responding; re-run the job."
      return 1
    fi

    ceiling="$(clamp_ceiling "$per_attempt")"
    echo "ci-apt-install: ${label}, attempt ${attempt}/${attempts} (${spent}s elapsed, ${ceiling}s ceiling)" >&2
    rc=0
    run_attempt "$ceiling" "$@" || rc=$?

    # `apt-get update` exits 0 when a source fails to fetch or resolve; it
    # only warns (`W: Failed to fetch …`, run 43037343501). Read the log rather
    # than pass `--error-on=any`, which needs apt >= 2.1 on every runner.
    if [ "$rc" -eq 0 ] && [ "${2:-}" = "update" ] &&
      grep -qiE '^Err:|failed to fetch|temporary failure resolving|could not resolve|some index files failed to download' "$ATTEMPT_LOG"; then
      echo "ci-apt-install: ${label} attempt ${attempt} exited 0 but logged a fetch failure; counting it as failed." >&2
      rc=100
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
    note_hosts "$label" "$attempt" "$rc" "$srcdir"

    delay=$(( RETRY_DELAY_S * attempt ))
    attempt=$((attempt + 1))
    if [ "$attempt" -le "$attempts" ]; then
      # #6064: repair before the retry — the poisoned attempts are in this loop.
      repair_dpkg
      [ "$reason" = "hash-mismatch" ] && clear_apt_lists
      [ "$delay" -gt 0 ] && sleep "$delay"
    fi
  done

  if [ -n "$last_reason" ]; then
    INFRA_REASON="$last_reason" INFRA_LABEL="$label" INFRA_MADE="$attempts" INFRA_RC="$rc"
    return "$INFRA_EXIT"
  fi

  PHASE_FAIL_MSG="::error::ci-apt-install: ${label} failed ${attempts} time(s), last exit ${rc}, $(elapsed)s elapsed. Exit 124 means the mirror stalled and the attempt was killed at its ${ceiling}s ceiling."
  return 1
}

# all_sources — every non-comment line of SOURCES_DIR's apt sources.
all_sources() {
  cat "${SOURCES_DIR}/sources.list" "${SOURCES_DIR}/sources.list.d/"*.list \
    "${SOURCES_DIR}/sources.list.d/"*.sources 2>/dev/null | grep -v '^[[:space:]]*#'
}

# mirror_file_re — an ERE matching each `mirror+file:<path>` URI whose mirror
# list names an Ubuntu archive host. The hosted runners point their Ubuntu
# sources at `mirror+file:/etc/apt/apt-mirrors.txt` (run 43037343501), so a
# rewrite of plain URIs alone finds nothing there. A path under /etc/apt is
# read from SOURCES_DIR first, so the self-test can supply the list.
mirror_file_re() {
  local uri path file re=""
  for uri in $(all_sources | grep -oE 'mirror\+file:[^[:space:]]+' | sed 's#/*$##' | sort -u); do
    path="${uri#mirror+file:}"
    file="$path"
    case "$path" in
      /etc/apt/*) [ -f "${SOURCES_DIR}/${path#/etc/apt/}" ] && file="${SOURCES_DIR}/${path#/etc/apt/}" ;;
    esac
    grep -qE "$UBUNTU_URI_RE" "$file" 2>/dev/null || continue
    re="${re:+${re}|}$(printf '%s' "$uri" | sed -E 's/[][\.*^$+?(){}|]/\\&/g')/?"
  done
  echo "$re"
}

# write_fallback_sources <mirror> <out-dir> — copy SOURCES_DIR's sources into
# <out-dir> with every Ubuntu archive URI, and every `mirror+file:` URI whose
# list names one, pointed at <mirror>. Returns 1, writing nothing useful, when
# no source line names an Ubuntu archive host either way.
write_fallback_sources() {
  local mirror="${1%/}/" out="$2" f rewritten=0 re mf_re
  mf_re="$(mirror_file_re)"
  re="${UBUNTU_URI_RE}${mf_re:+|${mf_re}}"
  rm -rf "$out"
  mkdir -p "${out}/parts"
  : >"${out}/sources.list"
  if [ -f "${SOURCES_DIR}/sources.list" ]; then
    sed -E "s#${re}#${mirror}#g" "${SOURCES_DIR}/sources.list" >"${out}/sources.list"
  fi
  for f in "${SOURCES_DIR}/sources.list.d/"*.list "${SOURCES_DIR}/sources.list.d/"*.sources; do
    [ -f "$f" ] || continue
    sed -E "s#${re}#${mirror}#g" "$f" >"${out}/parts/${f##*/}"
  done
  rewritten="$(all_sources | grep -cE "$re" || true)"
  [ "${rewritten:-0}" -gt 0 ] || return 1
  echo "$rewritten"
}

# ---------------------------------------------------------------------------
# Route 1: already installed.
# ---------------------------------------------------------------------------
PKGS=("$@")
if command -v dpkg-query >/dev/null 2>&1; then
  PKGS=()
  for pkg in "$@"; do
    pkg_satisfied "$pkg" || PKGS+=("$pkg")
  done
  if [ "${#PKGS[@]}" -eq 0 ]; then
    echo "ci-apt-install: already installed: $* — dpkg-query reports each one 'ok installed' (build-essential counts when ${BUILD_ESSENTIAL_PARTS} are). No apt network work." >&2
    exit 0
  fi
  echo "ci-apt-install: to install: ${PKGS[*]}" >&2
else
  echo "ci-apt-install: no dpkg-query on PATH — cannot check what is installed; installing every requested package." >&2
fi

INSTALL_ARGS=(install -y --no-install-recommends "${PKGS[@]}")

# ---------------------------------------------------------------------------
# Route 2: install from the package lists already on the runner.
# ---------------------------------------------------------------------------
DEADLINE_S="$PRIMARY_DEADLINE_S"
ceiling="$(clamp_ceiling "$CACHED_INSTALL_TIMEOUT_S")"
echo "ci-apt-install: cached-index install, attempt 1/1 — no 'apt-get update' first (${ceiling}s ceiling)" >&2
rc=0
run_attempt "$ceiling" apt-get "${INSTALL_ARGS[@]}" || rc=$?
if [ "$rc" -eq 0 ]; then
  echo "ci-apt-install: installed ${PKGS[*]} from the runner's existing package lists in $(elapsed)s; no apt-get update ran" >&2
  exit 0
fi
echo "ci-apt-install: cached-index install failed (exit ${rc}); refreshing the package lists." >&2
note_hosts "cached-index install" 1 "$rc" "$SOURCES_DIR"
[ "$rc" -ne 124 ] || repair_dpkg

# ---------------------------------------------------------------------------
# Route 3: the configured mirrors.
# ---------------------------------------------------------------------------
update_rc=0
run_phase "apt-get update" "$UPDATE_TIMEOUT_S" "$ATTEMPTS" "$SOURCES_DIR" \
  apt-get update -qq || update_rc=$?
if [ "$update_rc" -ne 0 ] && [ "$update_rc" -ne "$INFRA_EXIT" ]; then
  echo "$PHASE_FAIL_MSG"
  exit 1
fi
# An update that failed on an infra signature can still have refreshed the
# Ubuntu lists (a third-party source may be the one that failed), so the
# install runs once anyway. If it fails without a signature of its own — say
# `Unable to locate package` on lists that never landed — the update's infra
# verdict stands and route 4 runs.
install_attempts="$ATTEMPTS"
if [ "$update_rc" -eq "$INFRA_EXIT" ]; then
  U_REASON="$INFRA_REASON" U_LABEL="$INFRA_LABEL" U_MADE="$INFRA_MADE" U_RC="$INFRA_RC"
  install_attempts=1
fi
primary_rc=0
run_phase "apt-get install" "$INSTALL_TIMEOUT_S" "$install_attempts" "$SOURCES_DIR" \
  apt-get "${INSTALL_ARGS[@]}" || primary_rc=$?
if [ "$primary_rc" -eq 1 ] && [ "$update_rc" -eq "$INFRA_EXIT" ]; then
  INFRA_REASON="$U_REASON" INFRA_LABEL="$U_LABEL" INFRA_MADE="$U_MADE" INFRA_RC="$U_RC"
  primary_rc="$INFRA_EXIT"
fi
if [ "$primary_rc" -eq 0 ]; then
  echo "ci-apt-install: installed ${PKGS[*]} in $(elapsed)s" >&2
  exit 0
fi
if [ "$primary_rc" -ne "$INFRA_EXIT" ]; then
  # Not the mirror: a fallback mirror serves the same packages and would fail
  # the same way, so it is not tried.
  echo "$PHASE_FAIL_MSG"
  exit 1
fi

# ---------------------------------------------------------------------------
# Route 4: fallback mirrors outside *.ubuntu.com.
# ---------------------------------------------------------------------------
# The primary verdict is the one reported if every fallback fails too.
P_REASON="$INFRA_REASON" P_LABEL="$INFRA_LABEL" P_MADE="$INFRA_MADE" P_RC="$INFRA_RC"
DEADLINE_S="$TOTAL_BUDGET_S"
tried=""
for mirror in $FALLBACK_MIRRORS; do
  [ "$(elapsed)" -lt "$DEADLINE_S" ] || break
  fb_dir="${WORK_DIR}/fallback"
  if ! n="$(write_fallback_sources "$mirror" "$fb_dir")"; then
    echo "ci-apt-install: no source in ${SOURCES_DIR} names an Ubuntu archive host; there is nothing to point at a fallback mirror." >&2
    break
  fi
  tried="${tried:+${tried}, }${mirror}"
  echo "ci-apt-install: falling back to mirror ${mirror} (outside *.ubuntu.com) — ${n} Ubuntu source line(s) rewritten" >&2
  fb_opts=(-o "Dir::Etc::SourceList=${fb_dir}/sources.list"
    -o "Dir::Etc::SourceParts=${fb_dir}/parts"
    -o "APT::Get::List-Cleanup=0")
  # The update's own failure is not decisive: a non-Ubuntu source in the copy
  # can fail it while the mirror's indexes still land. The install decides.
  run_phase "fallback apt-get update (${mirror})" "$UPDATE_TIMEOUT_S" 1 "$fb_dir" \
    apt-get update -qq "${fb_opts[@]}" || true
  if run_phase "fallback apt-get install (${mirror})" "$INSTALL_TIMEOUT_S" 1 "$fb_dir" \
    apt-get "${INSTALL_ARGS[@]}" "${fb_opts[@]}"; then
    echo "ci-apt-install: installed ${PKGS[*]} from fallback mirror ${mirror} in $(elapsed)s; the primary mirror failed (${P_REASON}), host(s): ${FAILED_HOSTS:-none named}" >&2
    exit 0
  fi
done

INFRA_REASON="$P_REASON" INFRA_LABEL="$P_LABEL" INFRA_MADE="$P_MADE" INFRA_RC="$P_RC"
if [ -n "$tried" ]; then
  report_infra "fallback mirror(s) ${tried} failed too."
else
  report_infra "no fallback mirror was tried."
fi
exit "$INFRA_EXIT"
