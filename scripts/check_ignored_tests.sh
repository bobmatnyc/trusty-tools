#!/usr/bin/env bash
#
# check_ignored_tests.sh — run the `#[ignore]`d tests that a publish depends on,
# and account for every one it does not run.
#
# Why: no gate in this repo has ever executed an ignored test. `ci.yml`'s shard
#   matrix says so in its own header ("ONNX-backed tests are #[ignore]-tagged;
#   they are intentionally skipped here"), and nothing downstream picks them up.
#   The ONNX/embedder tests among them are the ones that exercise the model
#   loading path a published daemon runs on a user's machine at first start — so
#   the code most likely to break a fresh install is the code with the least
#   coverage before an irreversible upload.
#
# What: two halves that only make sense together.
#
#   THE RUN. `scripts/prepublish-ignored-tests.tsv` names the ignored tests this
#     gate executes — the ones that need nothing a hosted Linux runner cannot
#     provide (a HuggingFace download, and the model cache the workflow warms
#     before this step). It builds a nextest filterset from those rows and runs
#     them with `--run-ignored all`.
#
#   THE ACCOUNTING. Selecting a subset is a claim about the rest, and an
#     unexamined claim is how #5620 happened: a gate that runs 15 of 442 ignored
#     tests and prints a green tick has said nothing about 427 of them while
#     looking like it has. So this enumerates EVERY ignored test in the
#     workspace, subtracts the ones it ran, and holds the remainder against a
#     recorded baseline. The number is printed on every run, in full, grouped by
#     crate.
#
#   WHY A RATCHET AND NOT A COMPLETE MANIFEST. 442 ignored tests, 321 of them
#     carrying no reason string at all, is not a backlog one PR classifies — and
#     a gate that demands 442 justifications before it can pass would not ship.
#     The baseline makes the number VISIBLE and ONE-WAY: adding an ignored test
#     without classifying it turns this red. Lowering the baseline is always
#     welcome. This is the same shape as scripts/rustdoc-link-baseline.tsv and
#     the SLOC ratchet allowlist, for the same reason.
#
# WHAT THIS GATE DOES NOT CLAIM. It does not claim the unselected ignored tests
#   pass, and it never prints a line that could be read that way. Its summary
#   states both numbers — ran N, did not run M — because "0 failed" and
#   "0 examined" being printed with the same word is the defect this repo has
#   now been bitten by twice (#5620, #5723).
#
# FAIL-CLOSED BEHAVIOUR:
#   - A RUN row that matches ZERO tests FAILS. A renamed or deleted test would
#     otherwise silently shrink the gate while every row still looked present —
#     the stale-manifest failure mode, and the reason this is not just a
#     hand-written `-E` string in the workflow.
#   - An enumeration that lists zero ignored tests FAILS as a vacuous scan.
#   - An unselected count above the baseline FAILS.
#   - A nextest invocation that errors FAILS; a zero exit is never inferred.
#
# Exit codes: 0 = every selected test passed and the unselected count is at or
#   below baseline. 1 = a selected test failed, or the unselected count grew.
#   3 = the gate could not compute a verdict (vacuous enumeration, stale row,
#     nextest failure). 2 = usage error.
#
# Test: verified by construction against a real `cargo nextest list` stream
#   captured at e39183c3 (218 suites, 160 ignored tests) via `--from-list`,
#   which is the entry point a fixture-driven self-test would use. That run
#   confirmed 11 selected, 149 unselected, and every manifest row matching.
#   A self-test covering the STALE-ROW and VACUOUS branches is NOT yet written.

set -euo pipefail

usage() {
  echo "usage: scripts/check_ignored_tests.sh [--list-only] [--from-list <file>]" >&2
  echo "       --list-only        enumerate and reconcile; do not run any test" >&2
  echo "       --from-list <file> score a captured 'cargo nextest list" >&2
  echo "                          --message-format json' stream (self-test)" >&2
  exit 2
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
cd "$REPO_ROOT"

MANIFEST="${REPO_ROOT}/scripts/prepublish-ignored-tests.tsv"
LIST_ONLY=0
FROM_LIST=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --list-only) LIST_ONLY=1; shift ;;
    --from-list) FROM_LIST="${2:-}"; [ -n "$FROM_LIST" ] || usage; shift 2 ;;
    -h|--help) usage ;;
    *) echo "check_ignored_tests: unknown argument: $1" >&2; usage ;;
  esac
done

[ -f "$MANIFEST" ] || {
  echo "[FAIL] ignored-tests: manifest missing: ${MANIFEST}" >&2
  exit 3
}

# The four Tauri desktop crates, excluded for the same reason every headless
# workspace job in ci.yml excludes them: they need WebKit2GTK, and nothing here
# runs an ignored test of theirs. `trusty-audit-ui` was missing from this list
# and compiled on every run of this gate (#5891).
EXCLUDES=(
  --exclude trusty-mpm-gui
  --exclude trusty-code-gui
  --exclude trusty-agents-ui
  --exclude trusty-audit-ui
)

TMP_LIST="$(mktemp "${TMPDIR:-/tmp}/ignored-tests.list.XXXXXX")"
TMP_SEL="$(mktemp "${TMPDIR:-/tmp}/ignored-tests.sel.XXXXXX")"
trap 'rm -f "$TMP_LIST" "$TMP_SEL"' EXIT

# ---------------------------------------------------------------------------
# STEP 1 — enumerate every ignored test in the workspace.
# ---------------------------------------------------------------------------
if [ -n "$FROM_LIST" ]; then
  [ -f "$FROM_LIST" ] || { echo "check_ignored_tests: no such file: $FROM_LIST" >&2; exit 2; }
  cp "$FROM_LIST" "$TMP_LIST"
else
  echo "==> enumerating ignored tests (cargo nextest list --run-ignored ignored-only)" >&2
  # #5890: stderr used to go to /dev/null here, with no comment recorded
  # anywhere justifying the discard (checked git blame on this line — it has
  # had exactly one author, in the commit that introduced this file, #5731).
  # `--message-format json` on STDOUT is what the STEP 2 parser reads, so that
  # redirect stays; STDERR is left to inherit the script's own stderr instead.
  # Cargo's non-tty build output is one "Compiling <crate>" line per crate as
  # each finishes — exactly the periodic progress signal that tells a slow
  # build apart from a hang — and on a failure those lines are the compiler
  # diagnostics that used to be thrown away, already sitting in the job log
  # above the [FAIL] line below rather than needing to be captured and
  # replayed. Letting it inherit is not noise: it is the same build log any
  # plain `cargo build` in CI would print, and there is no cap to size because
  # nothing here re-buffers or truncates it.
  rc=0
  SKIP_UI_BUILD=1 cargo nextest list --workspace --locked "${EXCLUDES[@]}" \
    --run-ignored ignored-only --message-format json > "$TMP_LIST" || rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "[FAIL] ignored-tests: 'cargo nextest list' exited ${rc} — the gate enumerated nothing." >&2
    echo "       A zero count from a failed enumeration is not a finding. See the compiler output" >&2
    echo "       above for why the build failed, fix it, and re-run." >&2
    exit 3
  fi
fi

# ---------------------------------------------------------------------------
# STEP 2 — reconcile the enumeration against the manifest.
# Emits the nextest filterset for step 3 on fd 3 (TMP_SEL).
# ---------------------------------------------------------------------------
RECONCILE_RC=0
python3 - "$TMP_LIST" "$MANIFEST" "$TMP_SEL" <<'PY' || RECONCILE_RC=$?
import json, sys, collections, os, re

list_path, manifest_path, sel_path = sys.argv[1], sys.argv[2], sys.argv[3]

# --- Parse the manifest. Rows: <package>\t<exact test name>\t<why it is safe>
rows, baseline = [], None
with open(manifest_path) as fh:
    for line in fh:
        line = line.rstrip("\n")
        if line.startswith("# UNSELECTED-BASELINE:"):
            baseline = int(line.split(":", 1)[1].strip())
            continue
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        parts = line.split("\t")
        if len(parts) < 3:
            continue
        rows.append((parts[0].strip(), parts[1].strip(), parts[2].strip()))

if baseline is None:
    print("FAIL\tMANIFEST\tno '# UNSELECTED-BASELINE: <n>' line in the manifest — "
          "the gate cannot tell whether the unselected set grew")
    sys.exit(3)

# --- Parse the nextest enumeration.
all_tests = []   # (package, test-name)
with open(list_path) as fh:
    data = fh.read().strip()
try:
    doc = json.loads(data)
except json.JSONDecodeError:
    print("FAIL\tENUMERATION\tcould not parse the nextest list output as JSON")
    sys.exit(3)

for _bin_id, binfo in (doc.get("rust-suites") or {}).items():
    pkg = binfo.get("package-name") or ""
    for tname, tinfo in (binfo.get("testcases") or {}).items():
        # `ignored-only` already filtered, but assert rather than assume.
        if tinfo.get("ignored") is False:
            continue
        all_tests.append((pkg, tname))

if not all_tests:
    print("FAIL\tVACUOUS\tthe enumeration listed ZERO ignored tests — "
          "either the filter is wrong or nothing was built; both mean this gate examined nothing")
    sys.exit(3)

# --- Match manifest rows against the enumeration.
selected, stale = set(), []
for pkg, pattern, _why in rows:
    hits = [(p, t) for (p, t) in all_tests if p == pkg and re.fullmatch(pattern, t)]
    if not hits:
        stale.append(f"{pkg}\t{pattern}")
        continue
    selected.update(hits)

unselected = [t for t in all_tests if t not in selected]
by_crate = collections.Counter(p for p, _ in unselected)

print(f"SUMMARY\t{len(all_tests)} ignored test(s) in the workspace; "
      f"{len(selected)} selected to RUN; {len(unselected)} NOT run "
      f"(baseline {baseline})")
for crate, n in sorted(by_crate.items(), key=lambda kv: -kv[1]):
    print(f"UNSELECTED\t{crate}\t{n}")

failed = False

# FAIL CLOSED: a row that selects nothing is a manifest that has drifted from
# the tree. Left unreported it shrinks the gate invisibly.
for s in stale:
    print(f"FAIL\tSTALE-ROW\t{s} matched no ignored test — renamed, deleted, or "
          "no longer #[ignore]d. Fix or remove the row.")
    failed = True

if len(unselected) > baseline:
    print(f"FAIL\tRATCHET\t{len(unselected)} ignored test(s) are not run by this "
          f"gate, above the recorded baseline of {baseline}. Either add the new "
          "test to the manifest so the gate runs it, or raise the baseline in "
          "the same PR and say why in the PR body.")
    failed = True
elif len(unselected) < baseline:
    print(f"NOTE\tIMPROVED\t{len(unselected)} unselected vs baseline {baseline} "
          "— lower '# UNSELECTED-BASELINE:' in the manifest to lock the gain in")

# --- Emit the nextest filterset for the run phase.
with open(sel_path, "w") as fh:
    if selected:
        # nextest filterset syntax: `test(=<exact name>)` is an exact match,
        # scoped to the owning package so two crates with same-named tests
        # cannot select each other's.
        expr = " + ".join(f"(package({p}) and test(={t}))" for p, t in sorted(selected))
        fh.write(expr)

sys.exit(3 if stale else (1 if failed else 0))
PY

if [ "$RECONCILE_RC" -ne 0 ]; then
  echo "[FAIL] ignored-tests: reconciliation refused (exit ${RECONCILE_RC}) — see the FAIL lines above." >&2
  exit "$RECONCILE_RC"
fi

if [ "$LIST_ONLY" -eq 1 ] || [ -n "$FROM_LIST" ]; then
  echo "[PASS] ignored-tests: reconciliation only (--list-only); no test was executed." >&2
  exit 0
fi

# ---------------------------------------------------------------------------
# STEP 3 — run the selected tests.
# ---------------------------------------------------------------------------
EXPR="$(cat "$TMP_SEL")"
if [ -z "$EXPR" ]; then
  echo "[FAIL] ignored-tests: the manifest selected no tests at all — nothing would run." >&2
  exit 3
fi

echo "==> running $(grep -c 'package(' <<<"${EXPR//+/$'\n'}") selected ignored test(s)" >&2
rc=0
SKIP_UI_BUILD=1 cargo nextest run --workspace --locked "${EXCLUDES[@]}" \
  --run-ignored all --no-fail-fast -E "$EXPR" || rc=$?

if [ "$rc" -ne 0 ]; then
  echo "[FAIL] ignored-tests: a selected ignored test failed (nextest exit ${rc})." >&2
  exit 1
fi

echo "[PASS] ignored-tests: every selected ignored test passed." >&2
exit 0
