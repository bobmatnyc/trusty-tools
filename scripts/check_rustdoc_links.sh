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
# FEATURE LANES — the gap #7466 closed.
#   `cargo doc` resolves DEFAULT features only, so a module behind a non-default
#   feature was never documented and its links were never checked. The gate
#   reported `0 broken link(s)` for all 25 crates while
#   `cargo doc -p trusty-mcp --features config` exited 101 on an ambiguous link
#   inside the default-off `config` feature, and trusty-mcp 0.1.1 had already
#   burned a version number on that class of failure at release time.
#
#   `--all-features` cannot be the answer: trusty-common's `embedder-*` ORT
#   variants are mutually exclusive at the `ort-sys` level. So the extra lanes
#   are DECLARED in `scripts/rustdoc-doc-lanes.tsv`, one `cargo doc --workspace`
#   pass per lane id, and that file must account for EVERY declared feature of
#   every documented crate — as a lane member, as a `NO-CFG` row the gate
#   re-verifies each run, or as a `SKIP` row with a written reason. A crate that
#   gains a feature fails this gate until the feature is placed.
#
#   Findings are DEDUPLICATED across lanes by (crate, file:line, message): a
#   link in unconditional code is reported once per lane, and counting it per
#   lane would multiply the ratchet by the lane count.
#
# Exit codes: 0 = at or below baseline for every crate. 1 = a crate regressed,
#   or an unbaselined crate has findings. 3 = the gate could not compute a
#   verdict (build failure, vacuous scan, unattributable span, missing crate,
#   an uncovered or unbuildable feature lane) — distinguished from 1 so a caller
#   can tell "your links got worse" from "nothing was checked", the distinction
#   #5289 added to the semver gate. 2 = usage error.
#
# Test: `scripts/check_rustdoc_links_selftest.sh` drives every fail-closed
#   branch through the `--json` entry point with synthetic cargo streams, so it
#   needs no doc build. Its load-bearing cases are `cached-no-op` — a stream of
#   exclusively `fresh: true` doc artifacts with zero diagnostics and cargo exit
#   0, the exact shape that printed a pass, which must exit 3 — and the #7466
#   lane cases: an unbuildable lane whose stream is empty must fail
#   LANE-NOT-EXAMINED rather than skip, an uncovered feature must fail
#   FEATURE-UNCOVERED, and a broken link found only in a feature lane must reach
#   the verdict.

set -euo pipefail

usage() {
  echo "usage: scripts/check_rustdoc_links.sh [--update-baseline]" >&2
  echo "       [--json <file> [--lane <id>] [--lane-features <list>] [--cargo-rc <n>]]..." >&2
  echo "       --update-baseline   rewrite the baseline from this run's counts" >&2
  echo "       --json <file>       score a previously captured JSON stream" >&2
  echo "                           instead of running cargo doc (used by the" >&2
  echo "                           self-test). Repeatable: one --json per lane." >&2
  echo "       --lane <id>         name the lane the preceding --json stands" >&2
  echo "                           for (default 'default' for the first)" >&2
  echo "       --lane-features <list>  the <crate>/<feature> list that lane" >&2
  echo "                           claims to enable, so the per-lane" >&2
  echo "                           examination check has something to check" >&2
  echo "       --cargo-rc <n>      the cargo exit status to score the preceding" >&2
  echo "                           --json stream against (default 0)" >&2
  echo "" >&2
  echo "       LANES_OVERRIDE=<file>    read feature lanes from <file>" >&2
  echo "       METADATA_OVERRIDE=<file> read the feature inventory from a" >&2
  echo "                                captured 'cargo metadata' JSON" >&2
  echo "       BASELINE_OVERRIDE=<file> score against <file> as the baseline" >&2
  exit 2
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# BASELINE_OVERRIDE lets the self-test score fixtures against a baseline it
# controls rather than whatever the real one says this week.
BASELINE="${BASELINE_OVERRIDE:-${REPO_ROOT}/scripts/rustdoc-link-baseline.tsv}"
# The declared feature lanes (#7466). Overridable so the self-test can score
# fixtures against a lane set it controls.
LANES_FILE="${LANES_OVERRIDE:-${REPO_ROOT}/scripts/rustdoc-doc-lanes.tsv}"
UPDATE=0

# Captured streams, one record per lane, as parallel arrays: bash 3.2 has no
# associative arrays and this script runs on macOS system bash.
JSON_FILES=()
JSON_LANES=()
JSON_FEATS=()
JSON_RCS=()

while [ "$#" -gt 0 ]; do
  case "$1" in
    --update-baseline) UPDATE=1; shift ;;
    --json)
      [ -n "${2:-}" ] || usage
      JSON_FILES+=("$2")
      # The first captured stream defaults to the default lane; later ones must
      # be named, because an unnamed second lane cannot be checked for having
      # examined anything.
      if [ "${#JSON_FILES[@]}" -eq 1 ]; then JSON_LANES+=("default"); else JSON_LANES+=("lane$((${#JSON_FILES[@]} - 1))"); fi
      JSON_FEATS+=("")
      JSON_RCS+=(0)
      shift 2
      ;;
    --lane)
      [ -n "${2:-}" ] || usage
      [ "${#JSON_FILES[@]}" -gt 0 ] || { echo "check_rustdoc_links: --lane must follow a --json" >&2; usage; }
      JSON_LANES[$((${#JSON_FILES[@]} - 1))]="$2"
      shift 2
      ;;
    --lane-features)
      [ -n "${2:-}" ] || usage
      [ "${#JSON_FILES[@]}" -gt 0 ] || { echo "check_rustdoc_links: --lane-features must follow a --json" >&2; usage; }
      JSON_FEATS[$((${#JSON_FILES[@]} - 1))]="$2"
      shift 2
      ;;
    --cargo-rc)
      [ -n "${2:-}" ] || usage
      [ "${#JSON_FILES[@]}" -gt 0 ] || { echo "check_rustdoc_links: --cargo-rc must follow a --json" >&2; usage; }
      JSON_RCS[$((${#JSON_FILES[@]} - 1))]="$2"
      shift 2
      ;;
    -h|--help) usage ;;
    *) echo "check_rustdoc_links: unknown argument: $1" >&2; usage ;;
  esac
done

# The three Tauri GUI crates are excluded for the same reason every other
# workspace-wide job in this repo excludes them: they need WebKit2GTK, which no
# headless CI runner has. They are not published to crates.io, so they have no
# docs.rs page for a broken link to land on.
EXCLUDES=(--exclude trusty-mpm-gui --exclude trusty-code-gui --exclude trusty-agents-ui)

WORK="$(mktemp -d "${TMPDIR:-/tmp}/rustdoc-links.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

# The scorer's whole input: one row per lane, <lane-id> <json> <cargo-rc> <features>.
DESC="$WORK/lanes.tsv"
: > "$DESC"

# ---- The declared feature lanes (#7466) ---------------------------------
[ -f "$LANES_FILE" ] || {
  echo "check_rustdoc_links: no lane file at ${LANES_FILE} — the declared" >&2
  echo "       feature coverage is unknown, so this gate cannot report a verdict" >&2
  exit 3
}
LANE_IDS="$(awk -F'\t' '$1 == "LANE" { print $2 }' "$LANES_FILE" | LC_ALL=C sort -u)"

# The union of every LANE row carrying this id. Rows merge so a lane can be
# written one crate per row.
lane_features() {
  awk -F'\t' -v want="$1" \
    '$1 == "LANE" && $2 == want { printf "%s%s", sep, $3; sep = "," } END { printf "\n" }' \
    "$LANES_FILE"
}

# ---- The feature inventory the lanes must cover -------------------------
# A failure here is exit 3, never a pass: without the inventory the coverage
# check cannot run, and a gate that cannot check coverage has not checked it.
META_FILE="${METADATA_OVERRIDE:-}"
if [ -z "$META_FILE" ]; then
  META_FILE="$WORK/metadata.json"
  if ! cargo metadata --no-deps --format-version 1 > "$META_FILE" 2> "$WORK/metadata.err"; then
    echo "check_rustdoc_links: 'cargo metadata' failed — the declared feature" >&2
    echo "       inventory is unknown, so feature coverage cannot be checked:" >&2
    sed 's/^/       /' "$WORK/metadata.err" >&2
    exit 3
  fi
fi

if [ "${#JSON_FILES[@]}" -gt 0 ]; then
  i=0
  while [ "$i" -lt "${#JSON_FILES[@]}" ]; do
    f="${JSON_FILES[$i]}"
    [ -f "$f" ] || { echo "check_rustdoc_links: no such file: $f" >&2; exit 2; }
    printf '%s\t%s\t%s\t%s\n' \
      "${JSON_LANES[$i]}" "$f" "${JSON_RCS[$i]}" "${JSON_FEATS[$i]}" >> "$DESC"
    i=$((i + 1))
  done
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
  #
  # One pass per lane, default first so its fresh artifacts are what every
  # later lane's freshness is measured against. A lane's own exit status is
  # recorded rather than aggregated: the scorer needs to know WHICH lane failed.
  run_lane() {
    local lane="$1" feats="$2" out err rc=0
    case "$lane" in
      *[!A-Za-z0-9_-]*|"")
        echo "check_rustdoc_links: lane id '${lane}' is not [A-Za-z0-9_-]+" >&2
        exit 3
        ;;
    esac
    out="$WORK/${lane}.json"
    err="$WORK/${lane}.err"
    if [ -n "$feats" ]; then
      SKIP_UI_BUILD=1 RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" \
        cargo doc --workspace --no-deps --locked --keep-going \
          --message-format json "${EXCLUDES[@]}" --features "$feats" \
          > "$out" 2> "$err" || rc=$?
    else
      SKIP_UI_BUILD=1 RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" \
        cargo doc --workspace --no-deps --locked --keep-going \
          --message-format json "${EXCLUDES[@]}" \
          > "$out" 2> "$err" || rc=$?
    fi
    printf '%s\t%s\t%s\t%s\n' "$lane" "$out" "$rc" "$feats" >> "$DESC"
    echo "check_rustdoc_links: lane ${lane}: cargo exited ${rc}" >&2
  }

  run_lane default ""
  for lane in $LANE_IDS; do
    run_lane "$lane" "$(lane_features "$lane")"
  done
fi

# Every decision below is made by this Python block over the captured streams.
# It prints a machine-readable report on stdout that the shell then renders.
python3 - "$DESC" "$BASELINE" "$UPDATE" "$LANES_FILE" "$META_FILE" <<'PY'
import json, sys, collections, os, re

desc_path, baseline_path, update = sys.argv[1], sys.argv[2], sys.argv[3] == "1"
lanes_path, meta_path = sys.argv[4], sys.argv[5]

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
seen_findings = set()  # #7466 dedup key: the same link is re-reported per lane
hard_errors = []       # non-lint errors: the crate did not build
unattributable = []    # a diagnostic this script could not place
parsed_messages = 0
lane_documented = collections.defaultdict(set)   # lane id -> crates rustdoc ran for
lane_saw_diag = set()                            # lane id -> emitted any error diagnostic
lanes = []                                       # (id, json path, cargo rc, features)

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

with open(desc_path) as fh:
    for row in fh:
        row = row.rstrip("\n")
        if not row:
            continue
        parts = row.split("\t")
        while len(parts) < 4:
            parts.append("")
        lanes.append((parts[0], parts[1], int(parts[2] or 0), parts[3]))

for lane_id, json_path, lane_rc, _lane_feats in lanes:
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
                if m.get("fresh"):
                    cached.add(c)
                else:
                    documented.add(c)
                    # Per lane, because a lane proves nothing about a feature
                    # whose crate that lane did not re-document (#7466).
                    lane_documented[lane_id].add(c)
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
        # Recorded BEFORE the dedup below, so a lane whose only findings were
        # already seen in an earlier lane still counts as having spoken. This
        # is what keeps LANE-ERROR from firing on the ordinary "cargo exited
        # 101 because the lint denied a link" case.
        lane_saw_diag.add(lane_id)
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
            # Name every broken link. Reporting only a per-crate count leaves an
            # engineer with a number and no way to act on it — the Linux-only
            # links of #5753 had to be recovered by reading raw CI logs.
            loc = f'{primary[0]["file_name"]}:{primary[0].get("line_start", "?")}'
            # #7466: one link, one count. A link in unconditional code is
            # re-reported by every lane, so counting per lane would multiply
            # the ratchet by the lane count and make each new lane a
            # REGRESSED verdict on links that did not change.
            key = (crate, loc, text)
            if key in seen_findings:
                continue
            seen_findings.add(key)
            counts[crate] += 1
            findings.append((crate, loc, text))
        else:
            hard_errors.append(f"[{lane_id}] {crate}: {text}")

# ---- The lane declaration (#7466) --------------------------------------
# Parsed AFTER the streams so a malformed row is reported beside whatever the
# run found, not instead of it.
EXCLUDED_CRATES = {"trusty-mpm-gui", "trusty-code-gui", "trusty-agents-ui"}

lane_members = collections.defaultdict(set)   # lane id -> {crate}
declared = collections.defaultdict(set)       # crate -> {feature} placed by a row
no_cfg = collections.defaultdict(set)         # crate -> {feature} claimed cfg-free
skipped = collections.defaultdict(set)        # crate -> {feature} unbuildable
lane_rows_bad = []

def split_pair(token):
    if "/" not in token:
        return None, None
    c, _, f = token.partition("/")
    return c.strip(), f.strip()

with open(lanes_path) as fh:
    for raw in fh:
        raw = raw.rstrip("\n")
        if not raw or raw.lstrip().startswith("#"):
            continue
        cols = raw.split("\t")
        kind = cols[0].strip()
        if kind == "LANE":
            if len(cols) < 3 or not cols[1].strip() or not cols[2].strip():
                lane_rows_bad.append(f"LANE row needs <lane-id> and a feature list: {raw}")
                continue
            for token in cols[2].split(","):
                token = token.strip()
                if not token:
                    continue
                c, f = split_pair(token)
                if not c or not f:
                    lane_rows_bad.append(f"lane member must be <crate>/<feature>: {token}")
                    continue
                lane_members[cols[1].strip()].add(c)
                declared[c].add(f)
        elif kind in ("NO-CFG", "SKIP"):
            if len(cols) < 3 or not cols[1].strip() or not cols[2].strip():
                lane_rows_bad.append(f"{kind} row needs <crate> and <feature>: {raw}")
                continue
            c, f = cols[1].strip(), cols[2].strip()
            declared[c].add(f)
            if kind == "NO-CFG":
                no_cfg[c].add(f)
            else:
                # A reason is what makes a coverage hole reviewable; a SKIP row
                # without one is the "0 compared printed PASS" shape (#5620).
                if len(cols) < 4 or not cols[3].strip():
                    lane_rows_bad.append(f"SKIP {c}/{f} states no reason")
                skipped[c].add(f)
        else:
            lane_rows_bad.append(f"unknown row kind '{kind}': {raw}")

# ---- The inventory the declaration must account for --------------------
# Keyed by PACKAGE name, because `--features <pkg>/<feat>` is what cargo takes
# and a package name is not always its directory (`tga` lives in
# crates/trusty-git-analytics). `pkg_dir` carries the translation to the crate
# DIRECTORY, which is the key space the baseline and every diagnostic span use;
# a nested member folds into the top-level crate that ships it, exactly as
# `crate_of` already does for findings.
inventory = {}     # package name -> {declared non-default features}
pkg_src = {}       # package name -> its own src/ directory
pkg_dir = {}       # package name -> crate directory the baseline is keyed on
with open(meta_path) as fh:
    meta = json.load(fh)
for pkg in meta.get("packages") or []:
    name = pkg.get("name") or ""
    if name in EXCLUDED_CRATES:
        continue
    manifest = pkg.get("manifest_path") or ""
    rel = os.path.relpath(manifest, os.getcwd()) if manifest.startswith("/") else manifest
    pkg_dir[name] = crate_of(rel) or name
    feats = {f for f in (pkg.get("features") or {}) if f != "default"}
    if not feats:
        continue
    inventory[name] = feats
    pkg_src[name] = os.path.join(os.path.dirname(manifest), "src")

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
    # #7466: and refuse when a declared lane did not examine what it named. A
    # baseline written from a run whose feature lane never built records the
    # DEFAULT-feature count as the whole truth, which is the state this issue
    # was filed about — and a bad baseline outlives the run that wrote it.
    for lane_id, _p, _rc, _f in lanes:
        members = lane_members.get(lane_id) or set()
        if not members:
            continue
        want_dirs = {pkg_dir.get(p, p) for p in members}
        absent = sorted(want_dirs - lane_documented.get(lane_id, set()))
        if absent:
            print(f"FAIL\tLANE-NOT-EXAMINED\tlane '{lane_id}' did not re-document "
                  f"{', '.join(absent)} — refusing to write a baseline that would "
                  "record the default-feature count as the whole truth")
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
    rcs = ", ".join(f"{lid}={rc}" for lid, _p, rc, _f in lanes) or "no lane ran"
    failures.append(
        "VACUOUS-SCAN\trustdoc ran for ZERO crates "
        f"({len(cached)} served from cache, cargo exited [{rcs}]) — this gate "
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

# ---- FAIL CLOSED 6: a malformed lane declaration (#7466) ---------------
for bad in lane_rows_bad[:20]:
    failures.append(f"LANES-FILE\t{os.path.basename(lanes_path)}: {bad}")

# ---- FAIL CLOSED 7: a feature no row accounts for (#7466) --------------
# The whole point of the declaration: a crate that gains a feature fails here
# until the feature is placed in a lane, verified cfg-free, or skipped with a
# reason. Without this arm a new default-off module is invisible again the day
# it lands, which is the defect #7466 reported.
for pkg in sorted(inventory):
    missing = sorted(inventory[pkg] - declared.get(pkg, set()))
    for feat in missing:
        failures.append(
            f"FEATURE-UNCOVERED\t{pkg}/{feat} is declared in Cargo.toml but no "
            f"row in {os.path.basename(lanes_path)} places it — add it to a LANE, "
            "or record it as NO-CFG (gates no cfg site) or SKIP (with a reason)"
        )
    unknown = sorted(declared.get(pkg, set()) - inventory[pkg])
    for feat in unknown:
        failures.append(
            f"LANES-FILE\t{pkg}/{feat} is named in "
            f"{os.path.basename(lanes_path)} but {pkg} declares no such feature"
        )
for pkg in sorted(set(declared) - set(inventory)):
    failures.append(
        f"LANES-FILE\t{os.path.basename(lanes_path)} names package '{pkg}', which "
        "this workspace has no documented member for"
    )

# ---- FAIL CLOSED 8: a NO-CFG exemption that has rotted (#7466) ---------
# The row's claim is mechanical, so it is re-checked rather than trusted: a
# feature that has since gained a `cfg` site in its own crate's source now
# gates documentation and owes a lane.
for pkg in sorted(no_cfg):
    src = pkg_src.get(pkg)
    if not src or not os.path.isdir(src):
        failures.append(
            f"NO-CFG-STALE\t{pkg}: no src/ directory to verify the NO-CFG rows "
            "against, so the claim cannot be checked and is not granted"
        )
        continue
    blob = []
    for root, _dirs, files in os.walk(src):
        for fn in files:
            if not fn.endswith(".rs"):
                continue
            try:
                with open(os.path.join(root, fn), errors="ignore") as fh:
                    blob.append(fh.read())
            except OSError as exc:
                failures.append(
                    f"NO-CFG-STALE\t{pkg}: cannot read {os.path.join(root, fn)} "
                    f"({exc}) — the NO-CFG claim cannot be checked"
                )
    text_all = "\n".join(blob)
    for feat in sorted(no_cfg[pkg]):
        if re.search(r'feature\s*=\s*"' + re.escape(feat) + r'"', text_all):
            failures.append(
                f"NO-CFG-STALE\t{pkg}/{feat} is recorded as gating no cfg site, "
                "but its source now references it — it gates documentation and "
                "belongs in a LANE row"
            )

# ---- FAIL CLOSED 9: a lane that examined nothing it named (#7466) ------
# The anti-fail-open core of the feature phase. An unbuildable feature set,
# a typo in a lane row, or a cargo that died before invoking rustdoc all
# produce a lane whose crates were never re-documented — and a lane that did
# not run rustdoc for a crate has said nothing about that crate's links. It
# FAILS; it is never a skip.
for lane_id, _p, lane_rc, _f in lanes:
    members = lane_members.get(lane_id) or set()
    if not members:
        continue
    want_dirs = {pkg_dir.get(p, p) for p in members}
    absent = sorted(want_dirs - lane_documented.get(lane_id, set()))
    for crate in absent:
        failures.append(
            f"LANE-NOT-EXAMINED\tlane '{lane_id}' names features in {crate} but "
            f"rustdoc never re-documented it there (cargo exited {lane_rc}) — an "
            "unbuildable or mis-declared feature set is a failure, not a skip"
        )
    if lane_rc != 0 and not absent and lane_id not in lane_saw_diag:
        # cargo failed, every named crate was documented, and no diagnostic
        # explains it. Reporting a link count over that is the #5620 shape.
        failures.append(
            f"LANE-ERROR\tlane '{lane_id}': cargo exited {lane_rc} with no "
            "attributable error — the cause is unknown, so this run cannot "
            "report a verdict"
        )

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
# The lane inventory is part of the verdict, not trivia: "0 broken links" over
# the default feature set only is the claim #7466 was filed about.
feature_lanes = sorted(lane_members)
covered = sum(len(v) for v in inventory.values())
print(f"LANES\t{len(lanes)} pass(es): "
      + ", ".join(f"{lid}({len(lane_documented.get(lid, set()))} examined)"
                  for lid, _p, _rc, _f in lanes)
      + f"; {covered} declared feature(s) across {len(inventory)} crate(s), "
      + f"{sum(len(v) for v in no_cfg.values())} NO-CFG, "
      + f"{sum(len(v) for v in skipped.values())} SKIP"
      + (f"; feature lane(s): {', '.join(feature_lanes)}" if feature_lanes else ""))
print(f"SUMMARY\t{len(documented)} crate(s) examined by rustdoc, {total} broken "
      f"link(s), baseline {base_total}, {parsed_messages} diagnostic(s) examined"
      + (f", {len(cached)} crate(s) SERVED FROM CACHE" if cached else ""))
for crate, loc, text in findings:
    print(f"LINK\t{crate}\t{loc}\t{text}")
for n in notes:
    print(f"NOTE\t{n}")
for f in failures:
    print(f"FAIL\t{f}")

if any(f.startswith(("BUILD-ERROR", "VACUOUS-SCAN", "NOT-EXAMINED", "UNATTRIBUTABLE",
                     "NOT-DOCUMENTED", "LANES-FILE", "FEATURE-UNCOVERED",
                     "NO-CFG-STALE", "LANE-NOT-EXAMINED", "LANE-ERROR"))
       for f in failures):
    sys.exit(3)
sys.exit(1 if failures else 0)
PY
