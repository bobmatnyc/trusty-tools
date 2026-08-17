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
# FAIL-CLOSED BEHAVIOUR. Six distinct ways this refuses to report success:
#   1. A non-lint compile error (the crate does not build) FAILS, and says so
#      separately from a link count — "your docs are fine" is not a thing to say
#      about a crate that did not compile.
#   2. A run where rustdoc executed for ZERO crates FAILS as a vacuous scan,
#      whatever cargo's exit status was. See "WHAT COUNTS AS EXAMINED" below.
#   3. A crate whose doc unit was served from cache FAILS by name: rustdoc was
#      never invoked for it, so it could not have produced a diagnostic.
#   4. A crate that cargo never documented FAILS, even though its count is 0.
#      An absent crate's zero is not evidence about its links; this is the
#      "0 compared must never print PASS" invariant CHECK 5 of
#      preflight-publish.sh learned the hard way.
#   5. A diagnostic whose span this script cannot attribute to a crate FAILS
#      rather than being dropped.
#   6. A crate with findings but no baseline row FAILS.
#
# WHAT COUNTS AS EXAMINED — the #5620 shape, third instance.
#   Two things had to be true for a broken link to be counted, and the gate
#   checked neither. First, the old vacuous-scan guard fired only on
#   `parsed_messages == 0 AND cargo_rc != 0`: it asked whether cargo had
#   COMPLAINED, not whether rustdoc had RUN. Second, a warm target dir makes
#   `cargo doc` a no-op — cargo re-emits every `compiler-artifact` with
#   `"fresh": true` and invokes rustdoc for none of them. A cached run therefore
#   exits 0 with an empty diagnostic stream, satisfies neither half of the
#   guard, and prints a pass over nothing examined.
#
#   The diagnostic count cannot carry this check, because a genuinely clean tree
#   also has zero diagnostics. The evidence is the ARTIFACT: a `compiler-artifact`
#   whose filenames point into `target/doc/**/index.html` and whose `fresh` is
#   false means rustdoc actually executed for that crate. Zero of those is a
#   vacuous scan; the run also deletes the `doc-*` fingerprints up front so a
#   warm tree produces real evidence rather than a refusal.
#
#   Counting the wrong artifact was the other half of the illusion: every
#   workspace member is COMPILED as well as documented, and the rlib artifacts
#   outnumber the rustdoc ones. Scoring those is how a run in which rustdoc
#   executed for 3 crates reported "25 crate(s) documented".
#
# KNOWN LIMIT — features this gate cannot see.
#   `cargo doc` resolves DEFAULT features only, so a module behind a non-default
#   feature is never documented and its links are never checked. trusty-common's
#   `docgen` is the worked example: it is enabled only from `[dev-dependencies]`,
#   which `cargo doc` does not resolve, so `target/doc/trusty_common/docgen/`
#   is never created and two bare links lived there until `check_contracts.sh`
#   (which builds all 42 features) surfaced them. `--all-features` is not
#   available workspace-wide — trusty-common's three `embedder-*` ORT variants
#   are mutually exclusive. So this gate is sound for what it examines and
#   SILENT about the rest; it is not a whole-crate clean-tree assertion.
#
# Exit codes: 0 = at or below baseline for every crate. 1 = a crate regressed,
#   or an unbaselined crate has findings. 3 = the gate could not compute a
#   verdict (build failure, vacuous scan, unattributable span, missing crate) —
#   distinguished from 1 so a caller can tell "your links got worse" from
#   "nothing was checked", the distinction #5289 added to the semver gate.
#   2 = usage error.
#
# Test: `scripts/check_rustdoc_links_selftest.sh` drives every fail-closed
#   branch through the `--json` entry point with synthetic cargo streams, so it
#   needs no doc build. Its load-bearing case is `cached_no_op_is_not_a_pass`:
#   a stream of exclusively `fresh: true` doc artifacts with zero diagnostics
#   and cargo exit 0 — the exact shape that printed a pass — must exit 3.

set -euo pipefail

usage() {
  echo "usage: scripts/check_rustdoc_links.sh [--update-baseline] [--json <file>] [--cargo-rc <n>]" >&2
  echo "       --update-baseline   rewrite the baseline from this run's counts" >&2
  echo "       --json <file>       score a previously captured JSON stream" >&2
  echo "                           instead of running cargo doc (used by the" >&2
  echo "                           self-test)" >&2
  echo "       --cargo-rc <n>      with --json, the cargo exit status to score" >&2
  echo "                           the stream against (default 0)" >&2
  exit 2
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# BASELINE_OVERRIDE lets the self-test score fixtures against a baseline it
# controls rather than whatever the real one says this week.
BASELINE="${BASELINE_OVERRIDE:-${REPO_ROOT}/scripts/rustdoc-link-baseline.tsv}"
UPDATE=0
JSON_IN=""
FAKE_RC=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --update-baseline) UPDATE=1; shift ;;
    --json) JSON_IN="${2:-}"; [ -n "$JSON_IN" ] || usage; shift 2 ;;
    --cargo-rc) FAKE_RC="${2:-}"; [ -n "$FAKE_RC" ] || usage; shift 2 ;;
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
  CARGO_RC="$FAKE_RC"
else
  # Force rustdoc to actually RUN. cargo's freshness cache makes `cargo doc` a
  # no-op against a warm target dir: it re-emits every `compiler-artifact` with
  # `"fresh": true` and invokes rustdoc for none of them, so the run emits ZERO
  # diagnostics and the gate scored an empty stream as "0 broken links". That is
  # the #5620 shape — a PASS printed over nothing examined — and it is how 26
  # real Linux-only broken links read as green locally for a week.
  #
  # Deleting only the `doc-*` fingerprints invalidates the rustdoc units and
  # NOTHING else: dependency compilation stays cached, so this costs a rustdoc
  # pass (~40 s warm) rather than a full rebuild. CI needs it too — `rust-cache`
  # restores a warm target dir there, so a cached no-op is not a local-only risk.
  find "${CARGO_TARGET_DIR:-target}" -path '*/.fingerprint/*' -name 'doc-*' -delete 2>/dev/null || true

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
documented = set()     # rustdoc RAN for these crates (fresh=false doc artifact)
cached = set()         # rustdoc was SKIPPED for these (fresh=true doc artifact)
findings = []          # (crate, file:line, message) for every broken link
hard_errors = []       # non-lint errors: the crate did not build
unattributable = []    # a diagnostic this script could not place
parsed_messages = 0

LINT_PREFIXES = ("unresolved link to", "public documentation for")

def crate_of(path):
    parts = path.split("/")
    if path.startswith("crates/") and len(parts) > 2:
        return parts[1]
    return None

def is_doc_artifact(filenames):
    """A rustdoc output, as opposed to a compiled rlib for a dependency.

    `cargo doc` emits `compiler-artifact` for BOTH: every workspace member is
    also COMPILED (members depend on each other), and those rlib artifacts
    outnumber the rustdoc ones. Counting them is what let a fully cached run
    report "25 crate(s) documented" while rustdoc ran for three.
    """
    return any("/doc/" in f and f.endswith("index.html") for f in filenames)

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

        # Which crates rustdoc actually RAN for. `fresh` is the whole point:
        # a cached unit re-emits this message without invoking rustdoc, so it
        # cannot have produced a diagnostic and its zero is not evidence.
        if reason == "compiler-artifact":
            if not is_doc_artifact(m.get("filenames") or []):
                continue
            tgt = (m.get("target") or {})
            src = tgt.get("src_path") or ""
            c = crate_of(os.path.relpath(src, os.getcwd())) if src.startswith("/") else crate_of(src)
            if c:
                (cached if m.get("fresh") else documented).add(c)
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
            # Name every broken link. Reporting only a per-crate count leaves an
            # engineer with a number and no way to act on it — the Linux-only
            # links of #5753 had to be recovered by reading raw CI logs.
            loc = f'{primary[0]["file_name"]}:{primary[0].get("line_start", "?")}'
            findings.append((crate, loc, text))
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
    # Refuse to record a baseline from a run that examined nothing. #5744 drove
    # this baseline to zero from a macOS measurement that had already gone
    # vacuous, which is what made 26 broken links invisible: a bad baseline
    # outlives the run that wrote it.
    if not documented:
        print("FAIL\tVACUOUS-SCAN\trustdoc ran for ZERO crates — refusing to "
              f"write a baseline from a run that examined nothing ({len(cached)} "
              "crate(s) served from cache)")
        sys.exit(3)
    with open(baseline_path, "w") as fh:
        fh.write(HEADER)
        for crate in sorted(counts):
            fh.write(f"{crate}\t{counts[crate]}\n")
    print(f"BASELINE-UPDATED\t{len(counts)} crate(s) with findings, "
          f"{len(documented)} examined")
    sys.exit(0)

failures, notes = [], []

# ---- FAIL CLOSED 1: the crate did not build ----------------------------
if hard_errors:
    for e in hard_errors[:20]:
        failures.append(f"BUILD-ERROR\t{e}")

# ---- FAIL CLOSED 2: vacuous scan ---------------------------------------
# POSITIVE EVIDENCE, not an exit code. The old test was
# `parsed_messages == 0 and cargo_rc != 0`, which asked whether cargo COMPLAINED
# rather than whether rustdoc RAN. A fully cached `cargo doc` exits 0 and emits
# no diagnostics, so it satisfied neither half and scored as a clean pass over
# an empty stream — 26 real broken links read as "0 broken" for a week.
#
# The evidence that a scan happened is a crate rustdoc actually re-documented.
# Zero of those is a vacuous scan whatever cargo's status was, and a clean tree
# legitimately has zero DIAGNOSTICS, so the diagnostic count can never carry
# this check.
if not documented:
    failures.append(
        "VACUOUS-SCAN\trustdoc ran for ZERO crates "
        f"({len(cached)} served from cache, cargo exited {cargo_rc}) — this gate "
        "examined nothing and cannot report a verdict"
    )

# ---- FAIL CLOSED 2b: a partially cached run ----------------------------
# A crate whose doc unit was cached emitted no diagnostics because rustdoc was
# never invoked for it, not because its links are sound.
for crate in sorted(cached - documented):
    failures.append(
        f"NOT-EXAMINED\t{crate}: rustdoc was served from cache, so its zero "
        "findings are not evidence — delete target/**/.fingerprint/**/doc-* and rerun"
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
print(f"SUMMARY\t{len(documented)} crate(s) examined by rustdoc, {total} broken "
      f"link(s), baseline {base_total}, {parsed_messages} diagnostic(s) examined"
      + (f", {len(cached)} crate(s) SERVED FROM CACHE" if cached else ""))
for crate, loc, text in findings:
    print(f"LINK\t{crate}\t{loc}\t{text}")
for n in notes:
    print(f"NOTE\t{n}")
for f in failures:
    print(f"FAIL\t{f}")

if any(f.startswith(("BUILD-ERROR", "VACUOUS-SCAN", "NOT-EXAMINED", "UNATTRIBUTABLE", "NOT-DOCUMENTED"))
       for f in failures):
    sys.exit(3)
sys.exit(1 if failures else 0)
PY
