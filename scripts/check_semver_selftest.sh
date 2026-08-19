#!/usr/bin/env bash
#
# check_semver_selftest.sh — mutation self-test for the SemVer gate (issue #5050).
#
# Why: a gate that reports green when it could not do its job is worse than no
#   gate, because "SemVer check passed" then means "SemVer check was skipped".
#   scripts/check_semver.sh has exactly two ways to reach that state — a diff it
#   never scanned, and a crates.io probe it could not complete — and both are
#   ordinary-looking failure branches that a later refactor can turn back into
#   `|| continue`. This script re-proves in CI that neither one passes.
#
# What: four cases, each a mutation that a fail-open gate would report as OK.
#   1. SCAN FLOOR      — nothing in the diff. Must exit non-zero.
#   2. probe: refused  — the index host is unreachable (curl fails outright).
#   3. probe: HTTP 503 — the index answers, but not with an answer.
#   4. probe: HTTP 404 — the ONLY negative response that is a real fact about
#                        the crate. Must exit 0 reporting `baseline=NONE`, so
#                        this file also proves cases 2 and 3 fail for the right
#                        reason rather than because every probe path is broken.
#
#   Cases 2-4 drive the probe through `--probe`, which reaches the network
#   without a Cargo build, so the whole file runs in seconds.
#
#   Cases 5-8 (issue #5289) cover the defect that mattered more: the gate USED TO
#   print "a public API change requires a matching version bump" over a
#   cargo-semver-checks run that had failed to BUILD, so a rustdoc error and a
#   real SemVer break were indistinguishable from the output. These four pin the
#   classification apart:
#     5. real break          — exit 100 with a verdict. Must report BREAK.
#     6. rustdoc build error — exit 101, no comparison. Must report NO VERDICT
#                              and must NOT reach the version-bump remediation.
#     7. silent no-op        — exit 0, no comparison. Must still fail: "the tool
#                              said nothing" is not "the tool said pass".
#     8. clean               — exit 0 with a verdict. Must exit 0, which is what
#                              proves 5-7 fail on classification rather than
#                              because every path through the gate is broken.
#
#   Cases 5-8 replace only the `cargo semver-checks` SUBPROCESS, via a stub
#   `cargo` on PATH that forwards everything else (`metadata`) to the real one.
#   Everything under test — crate resolution, the registry probe, feature
#   enumeration, verdict classification, the messages, the exit status — is the
#   gate's own unmodified code. The stub replays FIXTURES captured verbatim from
#   real cargo-semver-checks 0.50.0 runs (only absolute paths rewritten to
#   /REPO); scripts/test-data/semver-gate/README.md records how each was taken.
#   silent-noop.out is the one synthetic fixture, because no real invocation
#   produces that shape today — it is the shape a future regression would take.
#
#   Cases 9-12 (issues #5296, #5297b) cover WHICH release the gate compares
#   against, and what it does when the bump is already breaking:
#     9.  post-publish        — the declared version is itself on crates.io, as
#                               it is by the time the tag workflow runs. The
#                               baseline must be the release BEFORE it; taking
#                               "latest" compares the crate against itself and
#                               reports no change forever.
#     10. first release       — nothing published below the declared version.
#                               A recorded skip, and no comparison attempted.
#     11. already-breaking    — must INVENTORY what the release breaks rather
#                               than skip, and must stay green doing it: the
#                               bump already permits the break.
#     12. inventory blind     — the advisory run failed. Still green, but it has
#                               to say so in the line AND the summary.
#   9, 11 and 12 fail against the pre-#5296 gate, which is what makes them
#   regression tests rather than descriptions of current behaviour.
#
#   Cases 16-18 (issue #5500) pin the gate against ANSI-COLOURED tool output.
#   Every fixture
#   above was captured by redirecting to a file on a workstation, where
#   cargo-semver-checks emits plain text — so `Checked` was always followed by a
#   space and the marker matched. In GitHub Actions it is not:
#   `dtolnay/rust-toolchain` exports CARGO_TERM_COLOR=always into $GITHUB_ENV,
#   every later step inherits it, and the summary line arrives as
#   `ESC[1mESC[32m     CheckedESC[0m [ 0.871s] 196 checks: ...`. The reset
#   sequence sits between `Checked` and the space, so a marker regex anchored on
#   `Checked<space>` sees nothing and reports a completed comparison as NO
#   VERDICT. Both new fixtures are verbatim CI captures of that shape:
#     16. coloured clean   — the tga run of PR #5458: exit 0, 196 pass. Reported
#                            as "exited 0 without completing a check run".
#     17. coloured break   — the same bytes at exit 100 through the PASS/FAIL
#                            arm. Must still be a BREAK, so the colour fix
#                            cannot be mistaken for a weakened gate.
#     18. coloured inventory — the trusty-review run of PR #5458: exit 100 with
#                            4 real failures, through the already-breaking arm.
#                            Reported as "exited 100 without completing a run".
#   16 and 17 ride the cases 5-8 table; 18 has its own block below. All three
#   fail against the pre-fix gate.
#
#     19. private-mode CSI — pins the strip to the WHOLE ECMA-48 CSI grammar
#                            rather than the SGR shape that was observed. Its
#                            fixture puts `ESC[?25l` (cursor hide, emitted by
#                            spinner renderers) between `Checked` and its space,
#                            which a `[0-9;]*[A-Za-z]` strip leaves in place —
#                            reintroducing this very defect. Synthetic, and
#                            labelled so: no cargo-semver-checks 0.50.0 output
#                            carries it. It rides the cases 5-8 table at exit
#                            100, so it also proves a private-mode-laden BREAK
#                            is still reported as a break rather than swallowed.
#
#   Cases 20-21 (issue #5440) pin the OTHER way a completed run says nothing.
#   cargo-semver-checks skips its whole lint set when the baseline -> current
#   delta already permits breakage, and prints
#       Checked [   0.000s] 0 checks: 0 pass, 254 skip
#       Summary no semver update required
#   at exit 0. The gate keyed its verdict on the summary line EXISTING, so it
#   read "254 lints declined" as "254 lints satisfied" and preflight-publish.sh
#   CHECK 5 passed having verified nothing:
#     20. zero checks / PASS-FAIL arm — must be NO VERDICT, exit 3, and must say
#                            it executed no checks rather than blaming rustdoc.
#     21. zero checks / INVENTORY arm — must be a BLIND inventory, never "no
#                            breaking changes found".
#   Case 22 pins the classification that decides WHICH arm a crate reaches:
#   every 0.0.z bump is breaking under Cargo's rules, so `release_type` must
#   call it major and route it to the advisory INVENTORY arm. Called `minor` it
#   lands in the PASS/FAIL arm, where the tool runs 0 of 254 checks and the gate
#   — correctly, per cases 20-21 — blocks the publish with an exit 3 that nothing
#   can clear.
#
#   Both fail against the pre-fix gate, which exits 0 on 20 and reports an empty
#   inventory on 21. The fixture is `all-skipped.out`, which is this file's own
#   former `clean.out`: the case that was supposed to prove the gate can pass a
#   crate was being satisfied by a run that checked nothing. `clean.out` is now a
#   real 196-pass capture, so "a real pass still exits 0" is still covered.
#
#   Cases 13-15 pin pre-release handling. `key()` used to strip the pre-release
#   suffix, so 1.0.0-rc1 and 1.0.0 both keyed to (1, 0, 0) and the tie went to
#   whichever the index listed first — the reproduction on record picked
#   1.0.0-rc1 as the baseline for a 1.0.1 release. 13 runs that exact history;
#   14 pins that a NEARER pre-release still loses to the last stable release;
#   15 pins that a skip caused by the exclusion names the version it refused.
#
#   Cases 23-24 pin the CRATE-exclusion arm
#   (scripts/semver-checks-crate-exclusions.tsv). A skip list stops protecting
#   silently — the day something depends on the excluded crate's library, the row
#   is wrong and nothing says so — so the gate re-checks the premise on every
#   run:
#     23. honoured        — trusty-mpm is a recorded skip naming the file it came
#                           from, and NO comparison is attempted.
#     24. premise died    — a workspace crate depends on the excluded one, so the
#                           skip is REFUSED (exit 3) and the dependent is named.
#   Case 8 is the other half: a non-excluded crate still runs a full clean
#   comparison with the real exclusions file in place.
#
# Usage:  bash scripts/check_semver_selftest.sh
# Exit:   0 when every case behaves; 1 (naming the case) when one does not.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI). Needs python3 for the
# stub index server; the gate needs it anyway.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
GATE="${REPO_ROOT}/scripts/check_semver.sh"

PASSED=0
FAILED=0
STUB_PID=""
cleanup() { [[ -n "$STUB_PID" ]] && kill "$STUB_PID" 2>/dev/null; return 0; }
trap cleanup EXIT

fail_case() {
  echo "SELF-TEST FAIL: $1" >&2
  shift
  printf '%s\n' "$@" | sed 's/^/       /' >&2
  FAILED=$((FAILED + 1))
}

pass_case() {
  echo "  ok  $1"
  PASSED=$((PASSED + 1))
}

# ===========================================================================
# 1. Scan floor — base == HEAD, so the diff lists nothing.
# ===========================================================================
out=""
rc=0
out="$(cd "$REPO_ROOT" && bash "$GATE" --base HEAD 2>&1)" || rc=$?
if [[ "$rc" -eq 0 ]]; then
  fail_case "scan floor: the gate exited 0 over a diff of 0 paths" "$out"
elif [[ "$out" != *"SCAN FLOOR"* ]]; then
  fail_case "scan floor: exited ${rc} but did not name SCAN FLOOR, so it may be failing for an unrelated reason" "$out"
else
  pass_case "scan floor rejects an empty diff (exit ${rc})"
fi

# ===========================================================================
# Stub crates.io index. Serves a status chosen by the path so cases 3 and 4
# share one server:  /503/...  -> 503        /404/...  -> 404
# ===========================================================================
PORT_FILE="$(mktemp "${TMPDIR:-/tmp}/semver-selftest.XXXXXX")"
python3 - "$PORT_FILE" > /dev/null 2>&1 <<'PY' &
import http.server, json, socketserver, sys, threading

class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        # /v/<v1>[,<v2>...]/... serves a sparse-index body naming each version
        # as a non-yanked release, so cases 5-8 get a real baseline without
        # touching the network and cases 9-11 (#5296) can serve a release
        # HISTORY — which is the only way to tell "compare against the latest"
        # apart from "compare against the previous". Everything else keeps the
        # original behaviour: /503 -> 503, anything else -> 404.
        parts = self.path.split("/")
        if len(parts) > 2 and parts[1] == "v":
            body = b"".join(
                json.dumps({"name": "stub", "vers": v, "yanked": False}).encode()
                + b"\n"
                for v in parts[2].split(",")
            )
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        code = 503 if self.path.startswith("/503") else 404
        self.send_response(code)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def log_message(self, *a):
        pass

with socketserver.TCPServer(("127.0.0.1", 0), H) as srv:
    with open(sys.argv[1], "w") as fh:
        fh.write(str(srv.server_address[1]))
    srv.serve_forever()
PY
STUB_PID=$!

for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
  [[ -s "$PORT_FILE" ]] && break
  sleep 0.25
done
PORT="$(cat "$PORT_FILE")"
rm -f "$PORT_FILE"
if [[ -z "$PORT" ]]; then
  echo "SELF-TEST FAIL: stub index server never reported a port." >&2
  exit 1
fi

# ===========================================================================
# 2. Connection refused — port 1 on loopback accepts nothing.
# ===========================================================================
rc=0
out="$(cd "$REPO_ROOT" && SEMVER_GATE_INDEX_BASE="http://127.0.0.1:1" \
  bash "$GATE" --probe trusty-common 2>&1)" || rc=$?
if [[ "$rc" -eq 0 ]]; then
  fail_case "probe/refused: an unreachable index exited 0 — the gate would report green while blind" "$out"
elif [[ "$out" != *"TOOL ERROR"* ]]; then
  fail_case "probe/refused: exited ${rc} without naming TOOL ERROR" "$out"
else
  pass_case "probe rejects an unreachable index (exit ${rc})"
fi

# ===========================================================================
# 3. HTTP 503 — the index answered, but said nothing about the crate.
# ===========================================================================
rc=0
out="$(cd "$REPO_ROOT" && SEMVER_GATE_INDEX_BASE="http://127.0.0.1:${PORT}/503" \
  bash "$GATE" --probe trusty-common 2>&1)" || rc=$?
if [[ "$rc" -eq 0 ]]; then
  fail_case "probe/503: a 503 from the index exited 0 — an outage would silently disable the gate" "$out"
elif [[ "$out" != *"TOOL ERROR"* ]]; then
  fail_case "probe/503: exited ${rc} without naming TOOL ERROR" "$out"
else
  pass_case "probe rejects HTTP 503 (exit ${rc})"
fi

# ===========================================================================
# 4. HTTP 404 — the one negative answer that IS an answer. Must be a clean,
#    reported skip, otherwise cases 2 and 3 prove nothing.
# ===========================================================================
rc=0
out="$(cd "$REPO_ROOT" && SEMVER_GATE_INDEX_BASE="http://127.0.0.1:${PORT}/404" \
  bash "$GATE" --probe never-published-crate 2>&1)" || rc=$?
if [[ "$rc" -ne 0 ]]; then
  fail_case "probe/404: an unpublished crate must be a recorded skip, not a failure (exit ${rc})" "$out"
elif [[ "$out" != *"baseline=NONE"* ]]; then
  fail_case "probe/404: exited 0 but did not report baseline=NONE" "$out"
else
  pass_case "probe treats HTTP 404 as 'never published' (exit 0, baseline=NONE)"
fi

# ===========================================================================
# 5-8. Verdict vs non-verdict (#5289).
#
# A stub `cargo` replaces ONLY the semver-checks subprocess; `metadata` and
# anything else forward to the real binary, so the gate runs its own code end to
# end. SEMVER_GATE_TOOLCHAIN_BIN= keeps select_toolchain from prepending a real
# toolchain's bin directory ahead of the stub — and doubles as coverage that the
# knob works.
# ===========================================================================
FIXTURES="${REPO_ROOT}/scripts/test-data/semver-gate"
STUB_DIR="$(mktemp -d "${TMPDIR:-/tmp}/semver-selftest-bin.XXXXXX")"
REAL_CARGO="$(command -v cargo)"
if [[ -z "$REAL_CARGO" ]]; then
  echo "SELF-TEST FAIL: no cargo on PATH; cases 5-8 need it for 'cargo metadata'." >&2
  exit 1
fi

cat > "${STUB_DIR}/cargo" <<'STUB'
#!/usr/bin/env bash
# Stub cargo for check_semver_selftest.sh: replays a captured cargo-semver-checks
# run, forwards everything else to the real cargo.
if [[ "${1:-}" == "semver-checks" ]]; then
  case " $* " in
    *" --version "*) echo "cargo-semver-checks 0.50.0"; exit 0 ;;
  esac

  # Build-acceleration probe (cases 25-27). Records what actually reached THIS
  # subprocess's environment, which is the only place the question "did the
  # wrapper get applied?" has an answer — the gate builds the assignments as an
  # `env` prefix, so nothing upstream of here can be inspected for it.
  if [[ -n "${SEMVER_SELFTEST_ENV_OUT:-}" ]]; then
    {
      echo "RUSTC_WRAPPER=${RUSTC_WRAPPER:-}"
      # PRESENCE, not value. `env -u CARGO_TARGET_DIR` and `CARGO_TARGET_DIR=`
      # both yield an empty ${CARGO_TARGET_DIR:-}, and only the first is
      # correct — cargo rejects an empty value outright. This stub replaces the
      # real cargo, so it is the only place that difference is observable at
      # all; recording `${VAR+x}` is what lets cases 25 and 28 tell them apart.
      if [[ -n "${CARGO_TARGET_DIR+x}" ]]; then
        echo "CARGO_TARGET_DIR_PRESENT=yes(${CARGO_TARGET_DIR})"
      else
        echo "CARGO_TARGET_DIR_PRESENT=no"
      fi
      echo "SKIP_UI_BUILD=${SKIP_UI_BUILD:-}"
    } > "$SEMVER_SELFTEST_ENV_OUT"
  fi

  fixture="${SEMVER_SELFTEST_FIXTURE:-}"
  rc="${SEMVER_SELFTEST_RC:-0}"

  # Baseline-keyed replay (#5296), "<version>=<file>:<rc>;..." — the fixture is
  # chosen by the --baseline-version the gate ASKED FOR. That is what makes the
  # post-publish case a real regression test: the old gate asks for the crate's
  # own version and gets a clean run, the new one asks for the release before it
  # and gets the break.
  if [[ -n "${SEMVER_SELFTEST_BY_BASELINE:-}" ]]; then
    want="" prev="" entry=""
    for a in "$@"; do
      [[ "$prev" == "--baseline-version" ]] && want="$a"
      prev="$a"
    done
    for e in ${SEMVER_SELFTEST_BY_BASELINE//;/ }; do
      case "$e" in
        "${want}="*) entry="${e#*=}" ;;
      esac
    done
    if [[ -z "$entry" ]]; then
      echo "stub cargo: no fixture registered for --baseline-version '${want}'" >&2
      exit 111
    fi
    fixture="${SEMVER_SELFTEST_FIXTURES}/${entry%%:*}"
    rc="${entry##*:}"
  fi

  cat "$fixture"
  exit "$rc"
fi
exec "$SEMVER_SELFTEST_REAL_CARGO" "$@"
STUB
chmod +x "${STUB_DIR}/cargo"

# The stub crate must be publishable, have a lib target, and — since #5296 — sit
# at a version that admits BOTH a same-minor predecessor (so cases 5-8 land in
# the normal PASS/FAIL arm) and a predecessor whose bump to it is already
# BREAKING (so cases 11, 12, 18 and 21 land in the INVENTORY arm). That means
# patch > 0 and minor > 0.
#
# WHICH PREDECESSOR IS BREAKING DEPENDS ON THE MAJOR (#5690). Under Cargo's 0.x
# rule a lower MINOR is breaking, but only while the major is 0; for a 1.x+ crate
# a lower minor is an ordinary `minor` release and the inventory arm is never
# reached. This picker hardcoded the 0.x form, which was invisible while
# trusty-common (0.33.1) won the preference below. #5717 moved it to 0.34.0,
# patch == 0 dropped it from the candidate list, and the fallback picked tga
# 2.19.1 — a 2.x crate, whose 2.18.0 baseline classifies `minor`. All four
# inventory cases then failed, blaming the gate for a harness assumption.
# Build the breaking predecessor from the same rule release_type applies:
# drop the major when there is one, otherwise drop the minor.
#
# trusty-common is preferred for continuity with the captured fixtures; any
# qualifying crate works, and none qualifying is a loud failure rather than a
# quietly degraded run.
PICK="$("$REAL_CARGO" metadata --no-deps --format-version 1 2>/dev/null | python3 -c '
import json, sys

cands = []
for p in json.load(sys.stdin)["packages"]:
    if p.get("publish") == []:
        continue
    kinds = {k for t in p["targets"] for k in t["kind"]}
    if not kinds & {"lib", "rlib", "cdylib", "proc-macro"}:
        continue
    core = p["version"].split("+")[0].split("-")[0].split(".")
    try:
        x, y, z = (int(c) for c in (core + ["0", "0", "0"])[:3])
    except ValueError:
        continue
    if y == 0 or z == 0:
        continue
    # #5690: a lower minor is breaking only under the 0.x rule. For a 1.x+ crate
    # the breaking position is the major, so drop that instead.
    breaking = "%d.0.0" % (x - 1) if x > 0 else "0.%d.0" % (y - 1)
    cands.append((p["name"], p["version"], "%d.%d.%d" % (x, y, z - 1), breaking))
cands.sort()
preferred = [c for c in cands if c[0] == "trusty-common"] or cands
if preferred:
    print(" ".join(preferred[0]))
')"
# shellcheck disable=SC2086
set -- $PICK
STUB_CRATE="${1:-}"
STUB_VERSION="${2:-}"
STUB_PREV="${3:-}"     # same minor, patch below -> a PATCH release
STUB_BREAKING="${4:-}" # major dropped (0.x: minor) -> already a MAJOR release
if [[ -z "$STUB_CRATE" || -z "$STUB_VERSION" || -z "$STUB_PREV" ]]; then
  echo "SELF-TEST FAIL: no publishable lib crate with minor>0 and patch>0 in this workspace;" >&2
  echo "                cases 5-11 cannot construct a baseline. Pick a crate by hand." >&2
  exit 1
fi
echo "stub crate: ${STUB_CRATE} v${STUB_VERSION} (prev ${STUB_PREV}, breaking base ${STUB_BREAKING})"

# The inventory cases below are only reached when the gate classifies
# STUB_BREAKING -> STUB_VERSION as `major`, so check it against the gate's own
# rule rather than assuming the construction above still matches it. #5690: when
# it silently stopped matching, four cases failed with messages naming the GATE
# ("the gate skipped instead of listing what the release breaks"), which is the
# wrong file to go looking in. release_type is lifted out of the gate BY PATTERN
# so this reads the shipped definition; case 22 exercises it directly.
eval "$(awk '/^release_type\(\) \{/,/^\}/' "$GATE")"
STUB_BREAKING_RTYPE="$(release_type "$STUB_BREAKING" "$STUB_VERSION")"
if [[ "$STUB_BREAKING_RTYPE" != "major" ]]; then
  echo "SELF-TEST FAIL: harness setup, not the gate — ${STUB_CRATE} ${STUB_BREAKING} -> ${STUB_VERSION}" >&2
  echo "                classifies '${STUB_BREAKING_RTYPE}', so cases 11, 12, 18 and 21 would exercise the" >&2
  echo "                PASS/FAIL arm instead of the INVENTORY arm they assert on. Fix the breaking-baseline" >&2
  echo "                construction in the stub-crate picker above (#5690)." >&2
  exit 1
fi

# name  fixture  stub-rc  expected-exit  must-contain  must-NOT-contain ("-" = none)
TAB="$(printf '\t')"
VERDICT_CASES="real break${TAB}break.out${TAB}100${TAB}1${TAB}VERDICT: BREAK${TAB}NO SEMVER VERDICT
rustdoc build error${TAB}build-error.out${TAB}101${TAB}3${TAB}NO SEMVER VERDICT WAS COMPUTED${TAB}requires a matching version bump
silent no-op at exit 0${TAB}silent-noop.out${TAB}0${TAB}3${TAB}NO SEMVER VERDICT WAS COMPUTED${TAB}requires a matching version bump
clean${TAB}clean.out${TAB}0${TAB}0${TAB}crate(s) checked${TAB}NO SEMVER VERDICT
ANSI-coloured clean (case 16)${TAB}clean-colored.out${TAB}0${TAB}0${TAB}crate(s) checked${TAB}NO SEMVER VERDICT
ANSI-coloured break (case 17)${TAB}break-colored.out${TAB}100${TAB}1${TAB}VERDICT: BREAK${TAB}NO SEMVER VERDICT
private-mode CSI break (case 19)${TAB}private-mode.out${TAB}100${TAB}1${TAB}VERDICT: BREAK${TAB}NO SEMVER VERDICT
zero checks executed (case 20)${TAB}all-skipped.out${TAB}0${TAB}3${TAB}NO SEMVER VERDICT WAS COMPUTED${TAB}requires a matching version bump"

while IFS="$TAB" read -r name fixture stub_rc want_exit must_have must_not; do
  [[ -z "$name" ]] && continue
  rc=0
  out="$(cd "$REPO_ROOT" && \
    PATH="${STUB_DIR}:${PATH}" \
    SEMVER_GATE_TOOLCHAIN_BIN='' \
    SEMVER_SELFTEST_REAL_CARGO="$REAL_CARGO" \
    SEMVER_SELFTEST_FIXTURE="${FIXTURES}/${fixture}" \
    SEMVER_SELFTEST_RC="$stub_rc" \
    SEMVER_GATE_INDEX_BASE="http://127.0.0.1:${PORT}/v/${STUB_PREV}" \
    bash "$GATE" --crate "$STUB_CRATE" 2>&1)" || rc=$?

  if [[ "$out" != *"CHECK ${STUB_CRATE}:"* ]]; then
    fail_case "verdict/${name}: the gate never reached the check — this case proves nothing. Did ${STUB_CRATE} start skipping?" "$out"
  elif [[ "$rc" -ne "$want_exit" ]]; then
    fail_case "verdict/${name}: expected exit ${want_exit}, got ${rc}" "$out"
  elif [[ "$out" != *"$must_have"* ]]; then
    fail_case "verdict/${name}: exit ${rc} but the output never said '${must_have}'" "$out"
  elif [[ "$must_not" != "-" && "$out" == *"$must_not"* ]]; then
    fail_case "verdict/${name}: output wrongly claims '${must_not}'" "$out"
  else
    pass_case "${name} -> exit ${rc}, says '${must_have}'"
  fi
done <<<"$VERDICT_CASES"

# --- 20b. The zero-check refusal must name its own cause. "it never ran" and
#          "it ran and skipped every lint" have different remedies, and a shared
#          message would send whoever hits #5440 hunting a rustdoc error that
#          does not exist.
rc=0
out="$(cd "$REPO_ROOT" && \
  PATH="${STUB_DIR}:${PATH}" \
  SEMVER_GATE_TOOLCHAIN_BIN='' \
  SEMVER_SELFTEST_REAL_CARGO="$REAL_CARGO" \
  SEMVER_SELFTEST_FIXTURE="${FIXTURES}/all-skipped.out" \
  SEMVER_SELFTEST_RC=0 \
  SEMVER_GATE_INDEX_BASE="http://127.0.0.1:${PORT}/v/${STUB_PREV}" \
  bash "$GATE" --crate "$STUB_CRATE" 2>&1)" || rc=$?
if [[ "$out" != *"EXECUTED NO CHECKS"* ]]; then
  fail_case "zero-checks/diagnosis: the refusal did not say the run executed no checks, so it is indistinguishable from a rustdoc failure" "$out"
elif [[ "$out" != *"0 checks: 0 pass, 254 skip"* ]]; then
  fail_case "zero-checks/diagnosis: the refusal did not quote the summary line it judged" "$out"
else
  pass_case "a zero-check refusal names the cause and quotes the summary (#5440)"
fi

# ===========================================================================
# 9-12. Which release is the baseline, and what happens when the bump is already
# breaking (#5296, #5297b).
#
# Case 9 is the regression test for #5296 and is written so it FAILS against the
# pre-fix gate: the stub index serves the crate's OWN version alongside the one
# before it — the state of the registry after `cargo publish`, which is when the
# tag-push workflow actually runs — and the stub replays a CLEAN run for the
# self-comparison and a BREAK for the real previous release. A gate that picks
# "latest" gets the clean fixture and exits 0 while a break is sitting there.
#
# Cases 11 and 12 pin the inventory arm: an already-breaking bump used to skip
# outright, so any assertion that the inventory ran fails against the old gate.
# ===========================================================================
gate_with_index() { # <comma-separated versions> <baseline=fixture:rc;...>
  (cd "$REPO_ROOT" &&
    PATH="${STUB_DIR}:${PATH}" \
      SEMVER_GATE_TOOLCHAIN_BIN='' \
      SEMVER_SELFTEST_REAL_CARGO="$REAL_CARGO" \
      SEMVER_SELFTEST_FIXTURES="$FIXTURES" \
      SEMVER_SELFTEST_BY_BASELINE="$2" \
      SEMVER_GATE_INDEX_BASE="http://127.0.0.1:${PORT}/v/${1}" \
      bash "$GATE" --crate "$STUB_CRATE" 2>&1)
}

# --- 9. The version under test is already published: compare against the one
#        before it, not against itself.
rc=0
out="$(gate_with_index "${STUB_PREV},${STUB_VERSION}" \
  "${STUB_VERSION}=clean.out:0;${STUB_PREV}=break.out:100")" || rc=$?
if [[ "$out" != *"CHECK ${STUB_CRATE}: ${STUB_PREV} -> ${STUB_VERSION}"* ]]; then
  fail_case "baseline/post-publish: the gate did not compare against v${STUB_PREV}. A baseline equal to the version under test is the #5296 self-comparison." "$out"
elif [[ "$rc" -ne 1 ]]; then
  fail_case "baseline/post-publish: expected exit 1 (the break against v${STUB_PREV}), got ${rc}" "$out"
elif [[ "$out" != *"VERDICT: BREAK"* ]]; then
  fail_case "baseline/post-publish: exit 1 but no BREAK verdict" "$out"
else
  pass_case "an already-published version is compared against the release BEFORE it (#5296)"
fi

# --- 10. Nothing published below the declared version: a recorded skip, and the
#         tool must not be invoked at all (any invocation hits the stub's
#         no-fixture path and says so).
rc=0
out="$(gate_with_index "${STUB_VERSION}" "unreachable=clean.out:0")" || rc=$?
if [[ "$rc" -ne 0 ]]; then
  fail_case "baseline/first-release: a crate with no earlier release must be a recorded skip, not a failure (exit ${rc})" "$out"
elif [[ "$out" != *"no previous release to compare against"* ]]; then
  fail_case "baseline/first-release: exited 0 without recording WHY nothing was compared" "$out"
elif [[ "$out" == *"CHECK ${STUB_CRATE}:"* ]]; then
  fail_case "baseline/first-release: the gate ran a comparison with no baseline to compare to" "$out"
else
  pass_case "no release below the declared version is a recorded skip (exit 0)"
fi

# --- 11. Already-breaking bump: an INVENTORY, not a skip, and advisory — the
#         listed breaks are permitted by the bump, so the gate stays green.
rc=0
out="$(gate_with_index "${STUB_BREAKING}" "${STUB_BREAKING}=break.out:100")" || rc=$?
if [[ "$out" != *"INVENTORY ${STUB_CRATE}: ${STUB_BREAKING} -> ${STUB_VERSION}"* ]]; then
  fail_case "inventory/already-breaking: the gate skipped instead of listing what the release breaks (#5297)" "$out"
elif [[ "$rc" -ne 0 ]]; then
  fail_case "inventory/already-breaking: the inventory is advisory and must not fail the gate (exit ${rc})" "$out"
elif [[ "$out" != *"breaking change(s) listed above"* ]]; then
  fail_case "inventory/already-breaking: exit 0 but the breaks were never reported" "$out"
elif [[ "$out" == *"VERDICT: BREAK"* ]]; then
  fail_case "inventory/already-breaking: a permitted break was rendered as a SemVer verdict" "$out"
else
  pass_case "an already-breaking bump is inventoried, advisory (#5297b)"
fi

# --- 12. The inventory itself could not run. Still advisory — but it must say
#         so, or "no inventory" would read as "inventory clean".
rc=0
out="$(gate_with_index "${STUB_BREAKING}" "${STUB_BREAKING}=build-error.out:101")" || rc=$?
if [[ "$rc" -ne 0 ]]; then
  fail_case "inventory/blind: a failed advisory run must not block an already-permitted release (exit ${rc})" "$out"
elif [[ "$out" != *"NO INVENTORY ${STUB_CRATE}"* ]]; then
  fail_case "inventory/blind: an inventory that never ran was not reported as such" "$out"
elif [[ "$out" != *"inventory NOT computed"* ]]; then
  fail_case "inventory/blind: the summary line hid the missing inventory" "$out"
elif [[ "$out" == *"no breaking changes found"* ]]; then
  fail_case "inventory/blind: a run that produced nothing was reported as clean" "$out"
else
  pass_case "an inventory that could not run says so, in the line and in the summary"
fi

# --- 21. The inventory arm over a run that executed no checks (#5440). Exit 0
#         with a summary, so the pre-fix gate reported "no breaking changes found
#         against vX" from a run that examined nothing — the advisory half of the
#         same false pass. It must land in the blind arm instead.
rc=0
out="$(gate_with_index "${STUB_BREAKING}" "${STUB_BREAKING}=all-skipped.out:0")" || rc=$?
if [[ "$rc" -ne 0 ]]; then
  fail_case "inventory/zero-checks: an advisory run must not block an already-permitted release (exit ${rc})" "$out"
elif [[ "$out" == *"no breaking changes found"* ]]; then
  fail_case "inventory/zero-checks: a run that executed 0 of 254 checks was reported as an empty inventory (#5440)" "$out"
elif [[ "$out" != *"NO INVENTORY ${STUB_CRATE}"* ]]; then
  fail_case "inventory/zero-checks: a run that examined nothing was not reported as such" "$out"
elif [[ "$out" != *"inventory NOT computed"* ]]; then
  fail_case "inventory/zero-checks: the summary line hid the missing inventory" "$out"
else
  pass_case "an inventory that executed no checks is blind, not empty (#5440)"
fi

# --- 18. The inventory arm over ANSI-coloured output. This is the trusty-review
#         half of PR #5458 verbatim: a completed run with 4 real failures that
#         the gate announced as "exited 100 without completing a run".
rc=0
out="$(gate_with_index "${STUB_BREAKING}" "${STUB_BREAKING}=break-colored.out:100")" || rc=$?
if [[ "$rc" -ne 0 ]]; then
  fail_case "inventory/coloured: an advisory run over coloured output must stay green (exit ${rc})" "$out"
elif [[ "$out" == *"NO INVENTORY ${STUB_CRATE}"* ]]; then
  fail_case "inventory/coloured: a completed run was reported as never having run — the marker did not survive the colour codes" "$out"
elif [[ "$out" != *"breaking change(s) listed above"* ]]; then
  fail_case "inventory/coloured: exit 0 but the breaks were never reported" "$out"
else
  pass_case "a coloured inventory run is recognised as completed"
fi

# ===========================================================================
# 13-15. Pre-releases are never a baseline (code-critic on PR #5445).
#
# Case 13 is the reproduction ON RECORD, run verbatim: the history
# ["0.9.9", "1.0.0-rc1", "1.0.0", "1.0.1-beta"] with declared version 1.0.1
# selected 1.0.0-rc1, because `key()` stripped the pre-release suffix and
# 1.0.0-rc1 / 1.0.0 tied at (1, 0, 0) with first-seen winning. `--probe-version`
# supplies the declared version so the case does not depend on any real crate's
# Cargo.toml sitting at 1.0.1.
#
# Case 15 is the fail-open trace: when the ONLY release below the declared
# version is a pre-release, the skip must NAME it. Sharing the generic "nothing
# below" message would hide a rejection behind a fact.
# ===========================================================================
CRITIC_HISTORY="0.9.9,1.0.0-rc1,1.0.0,1.0.1-beta"

# --- 13. The exact reproduction: the stable 1.0.0 must win over 1.0.0-rc1.
rc=0
out="$(cd "$REPO_ROOT" && SEMVER_GATE_INDEX_BASE="http://127.0.0.1:${PORT}/v/${CRITIC_HISTORY}" \
  bash "$GATE" --probe critic-repro-crate --probe-version 1.0.1 2>&1)" || rc=$?
if [[ "$rc" -ne 0 ]]; then
  fail_case "prerelease/repro: the probe exited ${rc}" "$out"
elif [[ "$out" == *"baseline=1.0.0-rc1"* ]]; then
  fail_case "prerelease/repro: selected the pre-release 1.0.0-rc1 over the stable 1.0.0 it shadows" "$out"
elif [[ "$out" != *"baseline=1.0.0 "* ]]; then
  fail_case "prerelease/repro: expected baseline=1.0.0 from ${CRITIC_HISTORY} declared 1.0.1" "$out"
else
  pass_case "the stable release wins the baseline slot over a pre-release with the same core"
fi

# --- 14. A pre-release ABOVE the last stable and below the declared version
#         (1.0.1-beta) is still not a baseline, and is reported as seen.
rc=0
out="$(cd "$REPO_ROOT" && SEMVER_GATE_INDEX_BASE="http://127.0.0.1:${PORT}/v/${CRITIC_HISTORY}" \
  bash "$GATE" --probe critic-repro-crate --probe-version 1.0.2 2>&1)" || rc=$?
if [[ "$rc" -ne 0 ]]; then
  fail_case "prerelease/nearest: the probe exited ${rc}" "$out"
elif [[ "$out" != *"baseline=1.0.0 "* ]]; then
  fail_case "prerelease/nearest: 1.0.1-beta is nearer to 1.0.2 than 1.0.0, but nothing resolves to it — expected baseline=1.0.0" "$out"
elif [[ "$out" != *"pre-release below=1.0.1-beta"* ]]; then
  fail_case "prerelease/nearest: the rejected pre-release was not reported" "$out"
else
  pass_case "a nearer pre-release does not displace the last stable release"
fi

# --- 15. Only a pre-release below the declared version: a skip that NAMES it,
#         never the generic "nothing below" line.
rc=0
out="$(gate_with_index "${STUB_PREV}-rc1" "unreachable=clean.out:0")" || rc=$?
if [[ "$rc" -ne 0 ]]; then
  fail_case "prerelease/only: a crate whose sole predecessor is a pre-release must be a recorded skip (exit ${rc})" "$out"
elif [[ "$out" != *"v${STUB_PREV}-rc1 is below it but a pre-release is never a baseline"* ]]; then
  fail_case "prerelease/only: the skip hid the pre-release it refused behind the generic 'nothing below' message" "$out"
elif [[ "$out" == *"CHECK ${STUB_CRATE}:"* ]]; then
  fail_case "prerelease/only: the gate compared against a pre-release" "$out"
else
  pass_case "the only-a-pre-release skip names what it refused"
fi

# ===========================================================================
# 22. release_type() over a 0.0.z crate (#5440, code-critic on PR #5522).
#
# Cargo gives a 0.0.z crate no compatible range, so every 0.0.z bump is
# breaking. Classified `minor`, such a crate lands in the PASS/FAIL arm, where
# cargo-semver-checks reads the same pair as a permitted break and runs 0 of 254
# checks — and since #5440 that is a correct NO VERDICT, i.e. a publish blocked
# by an exit 3 nothing can clear. `major` routes it to the advisory INVENTORY
# arm like every other permitted break.
#
# No crate in this workspace declares 0.0.z, so the end-to-end arms cannot reach
# it. The function is lifted out of the gate BY PATTERN rather than copied here,
# so this case exercises the shipped definition and not a stale duplicate of it.
# That lift now happens at the stub-crate pick above, which needs the same
# function to check its own breaking baseline (#5690).
# ===========================================================================

# baseline  current  expected
RELEASE_TYPE_CASES="0.0.5 0.0.6 major
0.0.5 0.1.0 major
0.30.1 0.31.0 major
0.31.0 0.31.1 minor
1.3.4 2.0.0 major
1.3.4 1.4.0 minor
1.3.4 1.3.5 patch"

rt_failed=0
while read -r rt_base rt_cur rt_want; do
  [[ -z "$rt_base" ]] && continue
  rt_got="$(release_type "$rt_base" "$rt_cur")"
  if [[ "$rt_got" != "$rt_want" ]]; then
    fail_case "release_type/${rt_base}->${rt_cur}: expected ${rt_want}, got ${rt_got}" \
      "A 0.0.z bump classified anything but 'major' lands in the PASS/FAIL arm, where the tool runs 0 checks and the gate blocks the publish with a NO VERDICT nothing can clear (#5440)."
    rt_failed=1
  fi
done <<<"$RELEASE_TYPE_CASES"
[[ "$rt_failed" -eq 0 ]] && pass_case "release_type calls every 0.0.z bump major, and still ranks 0.x/1.x correctly (#5440)"

# ===========================================================================
# 23-24. Crate exclusions, and the guard that keeps one from going silent.
#
# scripts/semver-checks-crate-exclusions.tsv skips a crate that has no LIBRARY
# consumer to protect — trusty-mpm is installed as the `tm` binary, and a binary
# user gets a whole new executable, so its library API compatibility protects
# nobody. That is a claim about consumption, and a skip list is exactly the shape
# that stops protecting silently: the day something depends on the excluded
# crate's library, the row is wrong and nothing says so.
#
#   23. the exclusion is honoured — a recorded skip, and NO comparison attempted.
#   24. the premise has died — a workspace crate depends on the excluded one, so
#       the skip is REFUSED (exit 3) and the dependent is named. Uses
#       trusty-agents-common, which trusty-agents, trusty-code and trusty-mpm all
#       depend on, against a fixture exclusions file; it fails the moment the
#       dependent check is deleted.
#
# Neither case reaches the network or cargo-semver-checks: the exclusion arm runs
# before the registry probe, which is also why an exclusion cannot be satisfied
# by a run that timed out.
#
# Case 8 above is the other half — a NON-excluded crate still runs a full clean
# comparison with the real exclusions file in place, so an exclusion that leaked
# to every crate fails there.
# ===========================================================================

# --- 23. trusty-mpm is excluded on main, and the skip attempts no comparison.
rc=0
out="$(cd "$REPO_ROOT" && bash "$GATE" --crate trusty-mpm 2>&1)" || rc=$?
if [[ "$rc" -ne 0 ]]; then
  fail_case "exclusion/honoured: an excluded crate must be a recorded skip (exit ${rc})" "$out"
elif [[ "$out" != *"SKIP trusty-mpm"* || "$out" != *"semver-checks-crate-exclusions.tsv"* ]]; then
  fail_case "exclusion/honoured: the skip did not name the crate and the file it came from" "$out"
elif [[ "$out" == *"CHECK trusty-mpm:"* ]]; then
  fail_case "exclusion/honoured: the gate compared a crate it had already excluded" "$out"
else
  pass_case "an excluded crate is skipped with its reason, and nothing is compared"
fi

# --- 24. An exclusion whose premise has died refuses the skip.
EXCL_FIXTURE="$(mktemp "${TMPDIR:-/tmp}/semver-selftest-excl.XXXXXX")"
printf '# fixture\ntrusty-agents-common\tfixture row: this crate HAS workspace dependents\n' \
  > "$EXCL_FIXTURE"
rc=0
out="$(cd "$REPO_ROOT" && SEMVER_GATE_CRATE_EXCLUSIONS="$EXCL_FIXTURE" \
  bash "$GATE" --crate trusty-agents-common 2>&1)" || rc=$?
rm -f "$EXCL_FIXTURE"
if [[ "$rc" -eq 0 ]]; then
  fail_case "exclusion/premise-died: the gate exited 0 over an exclusion that no longer holds — a silent coverage hole" "$out"
elif [[ "$rc" -ne 3 ]]; then
  fail_case "exclusion/premise-died: expected exit 3 (no verdict); got ${rc}" "$out"
elif [[ "$out" != *"EXCLUSION NO LONGER HOLDS"* ]]; then
  fail_case "exclusion/premise-died: exited 3 without naming the dead exclusion, so it may be failing for an unrelated reason" "$out"
elif [[ "$out" != *"trusty-code"* ]]; then
  fail_case "exclusion/premise-died: refused the skip without naming a dependent" "$out"
elif [[ "$out" == *"SKIP trusty-agents-common"* ]]; then
  fail_case "exclusion/premise-died: granted the skip anyway" "$out"
else
  pass_case "an exclusion contradicted by a workspace dependency refuses the skip (exit 3)"
fi

# ===========================================================================
# 25-27. Build acceleration reaches the subprocess, and cannot change a verdict.
#
# scripts/build_accel_selftest.sh already proves the RESOLVER answers correctly
# in every environment. What it cannot prove is that those answers are wired to
# anything: a resolver whose output the gate never applies passes every one of
# its cases. These three ask the wiring question instead, against the real gate
# and its real code path.
#
# The stub cargo writes its own environment to a file, so the assertion is on
# what the `cargo semver-checks` process actually received — not on a log line
# the gate printed about what it intended to send.
# ===========================================================================
ACCEL_SCCACHE_DIR="${STUB_DIR}/accel-bin"
mkdir -p "$ACCEL_SCCACHE_DIR"
printf '#!/bin/sh\nexec "$@"\n' > "${ACCEL_SCCACHE_DIR}/sccache"
chmod +x "${ACCEL_SCCACHE_DIR}/sccache"

ENV_OUT="${STUB_DIR}/accel-env.txt"

# accel_gate <extra-env-assignments...> — one gate run with the acceleration
# probe armed. The stub sccache goes on PATH AHEAD of the stub cargo's directory
# so neither the presence nor the absence of a real sccache install can decide
# the outcome on whoever's machine is running this.
accel_gate() {
  (cd "$REPO_ROOT" &&
    env "$@" \
      PATH="${ACCEL_SCCACHE_DIR}:${STUB_DIR}:${PATH}" \
      SEMVER_GATE_TOOLCHAIN_BIN='' \
      SEMVER_SELFTEST_REAL_CARGO="$REAL_CARGO" \
      SEMVER_SELFTEST_ENV_OUT="$ENV_OUT" \
      SEMVER_GATE_INDEX_BASE="http://127.0.0.1:${PORT}/v/${STUB_PREV}" \
      bash "$GATE" --crate "$STUB_CRATE" 2>&1)
}

# --- 25. The wrapper reaches the subprocess, and CARGO_TARGET_DIR does not.
#
#         The second half is the regression test, not a stylistic assertion. A
#         persistent CARGO_TARGET_DIR was built and then rejected on evidence:
#         cargo-semver-checks honours it in its INNER `cargo doc` too, which
#         moves the current crate's rustdoc JSON out of
#         semver-checks/local-<pkg>-<ver>-<triple>-<hash>/target/doc/ and into a
#         flat <target>/doc/<pkg>.json keyed by crate name alone. That blinds
#         check_semver_types.sh, and because the whole point of a persistent
#         directory is that worktrees share it, two parallel gate runs on the
#         same crate would overwrite each other's document — a stale cache
#         changing an answer. See scripts/lib/build_accel.sh for the full note.
rm -f "$ENV_OUT"
rc=0
out="$(accel_gate \
  "SEMVER_SELFTEST_FIXTURE=${FIXTURES}/clean.out" \
  "SEMVER_SELFTEST_RC=0")" || rc=$?
if [[ ! -f "$ENV_OUT" ]]; then
  fail_case "accel/applied: the gate never reached cargo semver-checks, so this case proves nothing" "$out"
elif [[ "$rc" -ne 0 ]]; then
  fail_case "accel/applied: a clean run under the wrapper exited ${rc}" "$out"
elif ! grep -qx "RUSTC_WRAPPER=${ACCEL_SCCACHE_DIR}/sccache" "$ENV_OUT"; then
  fail_case "accel/applied: RUSTC_WRAPPER never reached the subprocess" "$(cat "$ENV_OUT")" "$out"
elif ! grep -qx "CARGO_TARGET_DIR_PRESENT=no" "$ENV_OUT"; then
  fail_case "accel/applied: CARGO_TARGET_DIR reached cargo-semver-checks — it relocates the current rustdoc JSON and blinds the type differ" "$(cat "$ENV_OUT")"
elif ! grep -qx "SKIP_UI_BUILD=1" "$ENV_OUT"; then
  # The env prefix REPLACED an existing `SKIP_UI_BUILD=1 cargo ...` assignment.
  # Dropping it would trade a build-time saving for a UI build on every run.
  fail_case "accel/applied: the env prefix lost the pre-existing SKIP_UI_BUILD=1" "$(cat "$ENV_OUT")" "$out"
elif [[ "$out" != *"build-accel: sccache"* ]]; then
  fail_case "accel/applied: the run did not print its mode line" "$out"
else
  pass_case "sccache reaches the cargo subprocess, and CARGO_TARGET_DIR does not"
fi

# --- 26. The opt-out reaches it too. A knob proven only in the resolver is a
#         knob an operator cannot rely on mid-release.
rm -f "$ENV_OUT"
rc=0
out="$(accel_gate \
  "PREFLIGHT_NO_SCCACHE=1" \
  "SEMVER_SELFTEST_FIXTURE=${FIXTURES}/clean.out" \
  "SEMVER_SELFTEST_RC=0")" || rc=$?
if [[ ! -f "$ENV_OUT" ]]; then
  fail_case "accel/opt-out: the gate never reached cargo semver-checks" "$out"
elif ! grep -qx "RUSTC_WRAPPER=" "$ENV_OUT"; then
  fail_case "accel/opt-out: PREFLIGHT_NO_SCCACHE=1 did not stop the wrapper reaching cargo" "$(cat "$ENV_OUT")"
elif [[ "$rc" -ne 0 ]]; then
  fail_case "accel/opt-out: the opted-out run exited ${rc} where the accelerated one exited 0" "$out"
elif [[ "$out" != *"disabled by PREFLIGHT_NO_SCCACHE"* ]]; then
  fail_case "accel/opt-out: the mode line did not report the opt-out" "$out"
else
  pass_case "PREFLIGHT_NO_SCCACHE=1 reaches the subprocess and is reported"
fi

# --- 27. A FAILING BUILD UNDER THE WRAPPER STILL FAILS THE GATE.
#         This is the case the whole change is answerable for. A cache that can
#         turn a broken build into a skip, or into a pass, would be worse than
#         the rebuild it replaced — so the rustdoc-failure fixture is replayed
#         with the wrapper on, and the gate must reach exactly the exit 3 it
#         reaches without it.
rm -f "$ENV_OUT"
rc=0
out="$(accel_gate \
  "SEMVER_SELFTEST_FIXTURE=${FIXTURES}/build-error.out" \
  "SEMVER_SELFTEST_RC=101")" || rc=$?
if [[ "$rc" -eq 0 ]]; then
  fail_case "accel/build-failure: the gate PASSED a build failure that ran under the wrapper" "$out"
elif [[ "$rc" -ne 3 ]]; then
  fail_case "accel/build-failure: expected exit 3 (no verdict), got ${rc}" "$out"
elif [[ "$out" != *"NO SEMVER VERDICT WAS COMPUTED"* ]]; then
  fail_case "accel/build-failure: exited 3 without saying no verdict was computed" "$out"
elif [[ "$out" == *"requires a matching version bump"* ]]; then
  fail_case "accel/build-failure: rendered a build failure as a SemVer verdict" "$out"
elif ! grep -qx "RUSTC_WRAPPER=${ACCEL_SCCACHE_DIR}/sccache" "$ENV_OUT" 2> /dev/null; then
  fail_case "accel/build-failure: the run was not wrapped, so it did not test the wrapper" "$(cat "$ENV_OUT" 2> /dev/null)"
else
  pass_case "a build failure under the wrapper still exits 3 (no verdict)"
fi

# --- 28. AN AMBIENT CARGO_TARGET_DIR DOES NOT REACH THE SUBPROCESS.
#
#         Case 25 proves this gate does not ADD one. That is a weaker claim than
#         it looks, because the damage does not care who set the variable: an
#         inherited value relocates the current crate's rustdoc JSON to the same
#         flat, name-keyed path and blinds check_semver_types.sh exactly as a
#         locally-added one would.
#
#         And it is inheritable here in practice, not in theory. DOC-66 §6.2/§6.4
#         lists a shared CARGO_TARGET_DIR as a sanctioned opt-in disk-saving
#         strategy for this repo's worktrees, and vmtest-harness/lib/provision.sh
#         exports one specifically so it survives into cargo's child processes.
#         A developer who adopts either would silently lose CHECK 5b.
#
#         semver_checks_run uses `env -u`, not `CARGO_TARGET_DIR=`: cargo treats
#         an empty value as a hard error ("the target directory is set to an
#         empty string in the `CARGO_TARGET_DIR` environment variable", exit
#         101), so setting it empty would convert an ambient variable into a
#         failed gate rather than a clean one.
rm -f "$ENV_OUT"
rc=0
out="$(accel_gate \
  "CARGO_TARGET_DIR=${STUB_DIR}/ambient-target-dir" \
  "SEMVER_SELFTEST_FIXTURE=${FIXTURES}/clean.out" \
  "SEMVER_SELFTEST_RC=0")" || rc=$?
if [[ ! -f "$ENV_OUT" ]]; then
  fail_case "accel/ambient-target-dir: the gate never reached cargo semver-checks, so this case proves nothing" "$out"
elif grep -q "CARGO_TARGET_DIR_PRESENT=yes" "$ENV_OUT"; then
  # Catches BOTH wrong spellings, which is why the stub records presence rather
  # than value. No neutralisation at all leaves the ambient path visible;
  # `CARGO_TARGET_DIR=` leaves the variable present-but-empty, which cargo
  # rejects outright — and the stub replaces cargo, so an empty value would
  # otherwise sail through this file and fail only at a real release.
  fail_case "accel/ambient-target-dir: CARGO_TARGET_DIR still reached cargo-semver-checks — it must be UNSET (env -u), not set to empty" "$(cat "$ENV_OUT")"
elif [[ "$rc" -ne 0 ]]; then
  fail_case "accel/ambient-target-dir: neutralising the ambient value broke the run (exit ${rc})" "$out"
else
  pass_case "an ambient CARGO_TARGET_DIR is unset for the subprocess without failing the run"
fi

rm -rf "$STUB_DIR"

echo
if [[ "$FAILED" -ne 0 ]]; then
  echo "check_semver_selftest: ${PASSED} passed, ${FAILED} FAILED." >&2
  exit 1
fi
echo "check_semver_selftest: ${PASSED} passed, 0 failed."
