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
#   Cases 16-18 pin the gate against ANSI-COLOURED tool output. Every fixture
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
#   Cases 13-15 pin pre-release handling. `key()` used to strip the pre-release
#   suffix, so 1.0.0-rc1 and 1.0.0 both keyed to (1, 0, 0) and the tie went to
#   whichever the index listed first — the reproduction on record picked
#   1.0.0-rc1 as the baseline for a 1.0.1 release. 13 runs that exact history;
#   14 pins that a NEARER pre-release still loses to the last stable release;
#   15 pins that a skip caused by the exclusion names the version it refused.
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
# the normal PASS/FAIL arm) and a lower-minor one (so case 11 lands in the
# already-breaking INVENTORY arm). That means patch > 0 and minor > 0.
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
    cands.append((p["name"], p["version"], "%d.%d.%d" % (x, y, z - 1), "%d.%d.0" % (x, y - 1)))
cands.sort()
preferred = [c for c in cands if c[0] == "trusty-common"] or cands
if preferred:
    print(" ".join(preferred[0]))
')"
# shellcheck disable=SC2086
set -- $PICK
STUB_CRATE="${1:-}"
STUB_VERSION="${2:-}"
STUB_PREV="${3:-}"      # same minor, patch below  -> a PATCH release
STUB_OLD_MINOR="${4:-}" # lower minor             -> a MAJOR release under 0.x
if [[ -z "$STUB_CRATE" || -z "$STUB_VERSION" || -z "$STUB_PREV" ]]; then
  echo "SELF-TEST FAIL: no publishable lib crate with minor>0 and patch>0 in this workspace;" >&2
  echo "                cases 5-11 cannot construct a baseline. Pick a crate by hand." >&2
  exit 1
fi
echo "stub crate: ${STUB_CRATE} v${STUB_VERSION} (prev ${STUB_PREV}, older minor ${STUB_OLD_MINOR})"

# name  fixture  stub-rc  expected-exit  must-contain  must-NOT-contain ("-" = none)
TAB="$(printf '\t')"
VERDICT_CASES="real break${TAB}break.out${TAB}100${TAB}1${TAB}VERDICT: BREAK${TAB}NO SEMVER VERDICT
rustdoc build error${TAB}build-error.out${TAB}101${TAB}3${TAB}NO SEMVER VERDICT WAS COMPUTED${TAB}requires a matching version bump
silent no-op at exit 0${TAB}silent-noop.out${TAB}0${TAB}3${TAB}NO SEMVER VERDICT WAS COMPUTED${TAB}requires a matching version bump
clean${TAB}clean.out${TAB}0${TAB}0${TAB}crate(s) checked${TAB}NO SEMVER VERDICT
ANSI-coloured clean (case 16)${TAB}clean-colored.out${TAB}0${TAB}0${TAB}crate(s) checked${TAB}NO SEMVER VERDICT
ANSI-coloured break (case 17)${TAB}break-colored.out${TAB}100${TAB}1${TAB}VERDICT: BREAK${TAB}NO SEMVER VERDICT"

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
out="$(gate_with_index "${STUB_OLD_MINOR}" "${STUB_OLD_MINOR}=break.out:100")" || rc=$?
if [[ "$out" != *"INVENTORY ${STUB_CRATE}: ${STUB_OLD_MINOR} -> ${STUB_VERSION}"* ]]; then
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
out="$(gate_with_index "${STUB_OLD_MINOR}" "${STUB_OLD_MINOR}=build-error.out:101")" || rc=$?
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

# --- 18. The inventory arm over ANSI-coloured output. This is the trusty-review
#         half of PR #5458 verbatim: a completed run with 4 real failures that
#         the gate announced as "exited 100 without completing a run".
rc=0
out="$(gate_with_index "${STUB_OLD_MINOR}" "${STUB_OLD_MINOR}=break-colored.out:100")" || rc=$?
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

rm -rf "$STUB_DIR"

echo
if [[ "$FAILED" -ne 0 ]]; then
  echo "check_semver_selftest: ${PASSED} passed, ${FAILED} FAILED." >&2
  exit 1
fi
echo "check_semver_selftest: ${PASSED} passed, 0 failed."
