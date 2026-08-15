#!/usr/bin/env bash
#
# check_rustdoc_links.sh — broken intra-doc link gate for the publish path.
#
# Why: rustdoc renders `[`Foo`]` in a doc comment as a hyperlink when `Foo`
#   resolves and as dead literal text when it does not. docs.rs builds the
#   documentation for a crates.io release ONCE, from the uploaded tarball, and
#   never rebuilds it. So a broken link is not a bug that a later commit fixes —
#   it is baked into that version's published documentation forever, and the
#   only remedy is to publish a new version. Nothing in this repo checked for
#   them before this gate: `cargo doc` was absent from every workflow.
#
#   Measured on 2026-08-15 at e39183c3: 852 broken links across 16 of the 24
#   documented crates, including trusty-common (200), trusty-mpm (256),
#   trusty-code (125) and trusty-search (29) — all of which publish.
#
# What: runs `cargo doc --workspace --no-deps` with
#   `-D rustdoc::broken_intra_doc_links`, reads the diagnostics as STRUCTURED
#   JSON (never as scraped terminal text — see PARSING below), attributes each
#   one to the crate directory that owns the span, and compares the per-crate
#   totals against the recorded baseline in
#   `scripts/rustdoc-link-baseline.tsv`.
#
#   It is a RATCHET, not a clean-tree assertion. Fixing 852 links is not
#   something one PR does, and a gate that cannot pass is a gate that gets
#   switched off. So the baseline records where each crate stands today, the
#   gate fails when any crate gets WORSE, and lowering a baseline row is always
#   welcome and never demanded. A crate absent from the baseline must have ZERO,
#   which is what stops the ratchet from being a way to add new debt.
#
# PARSING: `--message-format json`, deliberately.
#   The first draft of this gate scraped `error:` / `-->` lines out of cargo's
#   human output with awk. Cross-checked against the JSON, that parser found 458
#   of the 852 real diagnostics — it silently dropped 46% of them, because a
#   rustdoc diagnostic block does not have the one-error-one-arrow shape the awk
#   assumed. A gate that under-reports by half while exiting 0 is precisely the
#   #5620 failure this repo has been bitten by twice. The JSON stream attributes
#   every diagnostic to a span with no heuristics; the checker below asserts
#   that it accounted for every one it saw.
#
# FAIL-CLOSED BEHAVIOUR. Five distinct ways this refuses to report success:
#   1. A non-lint compile error (the crate does not build) FAILS, and says so
#      separately from a link count — "your docs are fine" is not a thing to say
#      about a crate that did not compile.
#   2. A run that parsed ZERO compiler messages while cargo exited nonzero FAILS
#      as a vacuous scan. Zero findings and zero examined are different facts.
#   3. A crate that cargo never documented FAILS, even though its count is 0.
#      An absent crate's zero is not evidence about its links; this is the
#      "0 compared must never print PASS" invariant CHECK 5 of
#      preflight-publish.sh learned the hard way.
#   4. A diagnostic whose span this script cannot attribute to a crate FAILS
#      rather than being dropped.
#   5. A crate with findings but no baseline row FAILS.
#
# Exit codes: 0 = at or below baseline for every crate. 1 = a crate regressed,
#   or an unbaselined crate has findings. 3 = the gate could not compute a
#   verdict (build failure, vacuous scan, unattributable span, missing crate) —
#   distinguished from 1 so a caller can tell "your links got worse" from
#   "nothing was checked", the distinction #5289 added to the semver gate.
#   2 = usage error.
#
# Test: verified by construction against the real workspace at e39183c3 —
#   `--json` accepts a captured cargo stream, which is how the parser was
#   cross-checked (852 diagnostics attributed, 0 unattributable) and how the
#   NOT-DOCUMENTED branch was caught firing wrongly on crates whose doc build
#   FAILED and therefore emit no `compiler-artifact`. A fixture-driven self-test
#   covering the remaining fail-closed branches is NOT yet written; the `--json`
#   entry point exists so one can be added without a doc build.

set -euo pipefail

usage() {
  echo "usage: scripts/check_rustdoc_links.sh [--update-baseline] [--json <file>]" >&2
  echo "       --update-baseline   rewrite the baseline from this run's counts" >&2
  echo "       --json <file>       score a previously captured JSON stream" >&2
  echo "                           instead of running cargo doc (used by the" >&2
  echo "                           self-test)" >&2
  exit 2
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
cd "$REPO_ROOT"

BASELINE="${REPO_ROOT}/scripts/rustdoc-link-baseline.tsv"
UPDATE=0
JSON_IN=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --update-baseline) UPDATE=1; shift ;;
    --json) JSON_IN="${2:-}"; [ -n "$JSON_IN" ] || usage; shift 2 ;;
    -h|--help) usage ;;
    *) echo "check_rustdoc_links: unknown argument: $1" >&2; usage ;;
  esac
done

# The three Tauri GUI crates are excluded for the same reason every other
# workspace-wide job in this repo excludes them: they need WebKit2GTK, which no
# headless CI runner has. They are not published to crates.io, so they have no
# docs.rs page for a broken link to land on.
EXCLUDES=(--exclude trusty-mpm-gui --exclude trusty-code-gui --exclude trusty-agents-ui)

TMP_JSON="$(mktemp "${TMPDIR:-/tmp}/rustdoc-links.json.XXXXXX")"
TMP_ERR="$(mktemp "${TMPDIR:-/tmp}/rustdoc-links.err.XXXXXX")"
trap 'rm -f "$TMP_JSON" "$TMP_ERR"' EXIT

if [ -n "$JSON_IN" ]; then
  [ -f "$JSON_IN" ] || { echo "check_rustdoc_links: no such file: $JSON_IN" >&2; exit 2; }
  cp "$JSON_IN" "$TMP_JSON"
  : > "$TMP_ERR"
  CARGO_RC=0
else
  # --keep-going is what makes the inventory complete. Without it cargo stops at
  # the first crate whose docs fail, and the run reports only the crates it
  # happened to reach: the same command without it documented 4 of 24 crates and
  # found 152 of the 852 real findings. A gate that stops early does not
  # under-report politely — it reports a number that looks like an answer.
  CARGO_RC=0
  SKIP_UI_BUILD=1 RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" \
    cargo doc --workspace --no-deps --locked --keep-going \
      --message-format json "${EXCLUDES[@]}" \
      > "$TMP_JSON" 2> "$TMP_ERR" || CARGO_RC=$?
fi

# Every decision below is made by this Python block over the JSON stream. It
# prints a machine-readable report on stdout that the shell then renders.
python3 - "$TMP_JSON" "$BASELINE" "$UPDATE" "$CARGO_RC" <<'PY'
import json, sys, collections, os

json_path, baseline_path, update, cargo_rc = sys.argv[1], sys.argv[2], sys.argv[3] == "1", int(sys.argv[4])

HEADER = """# Per-crate broken intra-doc link baseline (scripts/check_rustdoc_links.sh).
#
# A RATCHET, not a target. Each row records how many broken links a crate has
# today; the gate fails when a crate exceeds its row, when a crate with no row
# has any at all, or when a crate that has a row was never documented.
#
# LOWERING A ROW IS ALWAYS WELCOME AND NEVER REQUIRED. Fix links in the PR that
# touches the file anyway, then run:
#     bash scripts/check_rustdoc_links.sh --update-baseline
# Deleting a row entirely is the goal state for every crate here.
#
# RAISING A ROW IS A REVIEW DECISION, not a way to make a red gate green. A
# broken link on docs.rs cannot be fixed without publishing a new version, so
# the cost of letting one through is a wasted version number.
#
# Regenerate with: bash scripts/check_rustdoc_links.sh --update-baseline
# Format (tab-separated):  <crate-directory>\t<broken-link-count>
"""

counts = collections.Counter()
documented = set()
hard_errors = []       # non-lint errors: the crate did not build
unattributable = []    # a diagnostic this script could not place
parsed_messages = 0

LINT_PREFIXES = ("unresolved link to", "public documentation for")

def crate_of(path):
    parts = path.split("/")
    if path.startswith("crates/") and len(parts) > 2:
        return parts[1]
    return None

with open(json_path) as fh:
    for line in fh:
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            m = json.loads(line)
        except json.JSONDecodeError:
            continue
        reason = m.get("reason")

        # Which crates cargo actually documented. Used for the fail-closed
        # "a crate we never built cannot be reported as clean" check.
        if reason == "compiler-artifact":
            tgt = (m.get("target") or {})
            name = tgt.get("name")
            src = tgt.get("src_path") or ""
            c = crate_of(os.path.relpath(src, os.getcwd())) if src.startswith("/") else crate_of(src)
            if c:
                documented.add(c)
            continue

        if reason != "compiler-message":
            continue
        msg = m.get("message") or {}
        if msg.get("level") != "error":
            continue
        text = msg.get("message") or ""

        # `could not document X` is cargo's roll-up of the per-link errors it
        # already emitted. Counting it would double-count the crate.
        if text.startswith("could not document"):
            continue

        parsed_messages += 1
        spans = msg.get("spans") or []
        primary = [s for s in spans if s.get("is_primary")] or spans
        if not primary:
            unattributable.append(text)
            continue
        crate = crate_of(primary[0]["file_name"])
        if crate is None:
            unattributable.append(f'{text} @ {primary[0]["file_name"]}')
            continue

        # A rustdoc lint error names the broken link; anything else at error
        # level means the crate genuinely failed to compile, which is a
        # different failure and must not be scored as a link count.
        code = ((msg.get("code") or {}) or {}).get("code") or ""
        is_link_lint = (
            code.startswith("rustdoc::")
            or text.startswith(LINT_PREFIXES)
            or " is both " in text          # ambiguous intra-doc link
            or "no item named" in text
        )
        if is_link_lint:
            counts[crate] += 1
        else:
            hard_errors.append(f"{crate}: {text}")

report = {
    "counts": dict(counts),
    "documented": sorted(documented),
    "hard_errors": hard_errors,
    "unattributable": unattributable,
    "parsed_messages": parsed_messages,
    "cargo_rc": cargo_rc,
}

# ---- Baseline I/O -------------------------------------------------------
baseline = {}
if os.path.exists(baseline_path):
    with open(baseline_path) as fh:
        for line in fh:
            line = line.rstrip("\n")
            if not line or line.lstrip().startswith("#"):
                continue
            parts = line.split("\t")
            if len(parts) < 2:
                continue
            try:
                baseline[parts[0]] = int(parts[1])
            except ValueError:
                continue

if update:
    with open(baseline_path, "w") as fh:
        fh.write(HEADER)
        for crate in sorted(counts):
            fh.write(f"{crate}\t{counts[crate]}\n")
    print(f"BASELINE-UPDATED\t{len(counts)}")
    sys.exit(0)

failures, notes = [], []

# ---- FAIL CLOSED 1: the crate did not build ----------------------------
if hard_errors:
    for e in hard_errors[:20]:
        failures.append(f"BUILD-ERROR\t{e}")

# ---- FAIL CLOSED 2: vacuous scan ---------------------------------------
if parsed_messages == 0 and cargo_rc != 0:
    failures.append(
        "VACUOUS-SCAN\tcargo doc exited "
        f"{cargo_rc} but this gate parsed ZERO diagnostics — it examined nothing"
    )

# ---- FAIL CLOSED 4: unattributable diagnostics -------------------------
for u in unattributable[:20]:
    failures.append(f"UNATTRIBUTABLE\t{u}")

# ---- FAIL CLOSED 3: a baselined crate cargo never examined -------------
# A crate counts as EXAMINED if rustdoc produced an artifact for it OR emitted
# a diagnostic against it. The second arm is not redundant: a crate whose doc
# build FAILS emits no `compiler-artifact` message at all, so an artifact-only
# test would report the crates with findings as un-examined — which is exactly
# backwards, and is what the first draft of this check did.
examined = documented | set(counts)
for crate in sorted(baseline):
    if crate not in examined:
        failures.append(
            f"NOT-DOCUMENTED\t{crate} has a baseline row but cargo never "
            "documented it — its zero findings are not evidence"
        )

# ---- FAIL CLOSED 5 + the ratchet itself --------------------------------
for crate in sorted(counts):
    have = counts[crate]
    if crate not in baseline:
        failures.append(
            f"UNBASELINED\t{crate} has {have} broken link(s) and no baseline "
            "row — a crate not in the baseline must have zero"
        )
    elif have > baseline[crate]:
        failures.append(
            f"REGRESSED\t{crate}: {have} broken link(s), baseline "
            f"{baseline[crate]} (+{have - baseline[crate]})"
        )
    elif have < baseline[crate]:
        notes.append(
            f"IMPROVED\t{crate}: {have} broken link(s), baseline "
            f"{baseline[crate]} (-{baseline[crate] - have}) — lower the "
            "baseline row to lock the gain in"
        )

for crate in sorted(baseline):
    if crate not in counts and crate in examined and baseline[crate] > 0:
        notes.append(
            f"IMPROVED\t{crate}: 0 broken links, baseline {baseline[crate]} "
            "— remove the baseline row to lock the gain in"
        )

total = sum(counts.values())
base_total = sum(baseline.values())
print(f"SUMMARY\t{len(documented)} crate(s) documented, {total} broken link(s), "
      f"baseline {base_total}, {parsed_messages} diagnostic(s) examined")
for n in notes:
    print(f"NOTE\t{n}")
for f in failures:
    print(f"FAIL\t{f}")

if any(f.startswith(("BUILD-ERROR", "VACUOUS-SCAN", "UNATTRIBUTABLE", "NOT-DOCUMENTED")) for f in failures):
    sys.exit(3)
sys.exit(1 if failures else 0)
PY
