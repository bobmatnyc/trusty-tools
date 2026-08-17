#!/usr/bin/env bash
#
# publish-dry-run-order_selftest.sh — the graph half of the publish-order
# preflight (issue #5358).
#
# Why: scripts/publish-dry-run-order.sh decides what order crates are published
#   in, and `cargo publish` cannot be undone — a crate uploaded before a sibling
#   it needs is fixed by a yank. The order is only as good as the graph it is
#   computed from, and the graph was wrong twice, in two independent ways that
#   each produced a confident, exit-0, plausible-looking order:
#
#     1. `cargo metadata` ran without --all-features, so optional and
#        feature-gated sibling edges were absent from `resolve` entirely.
#        trusty-analyze reaches trusty-review only through its optional
#        `review`-gated dependency, so the printed order put trusty-analyze
#        FIRST. Caught on 2026-08-10 by a dry-run that failed, not by the order.
#     2. `remaining = dict(edges)` was a shallow copy, so Kahn's
#        `difference_update` emptied the very sets `edges` holds. The
#        single-crate filter walks `edges` afterwards to build the publishable
#        closure, so it found nothing and emitted the requested crate ALONE.
#        That is the form release.yml's `publish-dry-run` job runs, so the job
#        dry-ran one crate against whatever happened to be live already and
#        checked none of its siblings.
#
#   Neither failure is visible in an exit status: the pre-fix script exits 0 on
#   every case below. Only the ORDER shows it, which is why this file asserts on
#   the order rather than on the status.
#
# What: runs the real script against a checked-in cargo-metadata fixture
#   (scripts/test-data/publish-dry-run-order/metadata.json) with a `cargo` shim
#   earlier on PATH, and asserts the emitted order. The shim models the one
#   behaviour that matters here — WITHOUT --all-features it strips the optional
#   edge from the graph, exactly as real cargo does — so case 1 tests the flag
#   itself rather than trusting that it was passed.
#
#   Always --list-only: no `cargo publish` ever runs, nothing touches crates.io,
#   and the whole file finishes in well under a second.
#
#   A fixture rather than a live `cargo metadata` run, deliberately: today
#   trusty-analyze -> trusty-review is the only optional internal edge in the
#   real workspace, and a test resting on that would go vacuous the moment
#   someone made that dependency unconditional. See the fixture's own _comment
#   for the crate-by-crate mapping back to the real graph.
#
# Usage:
#   bash scripts/publish-dry-run-order_selftest.sh                 # every case
#   bash scripts/publish-dry-run-order_selftest.sh optional_dep_order
#   bash scripts/publish-dry-run-order_selftest.sh --script /tmp/old.sh
#                                                  # drive a DIFFERENT copy of
#                                                  # the script, e.g. the
#                                                  # pre-fix one out of git, to
#                                                  # confirm these cases still
#                                                  # reproduce the defect
#   (names: optional_dep_order filter_closure dev_dep_edge)
#
# Exit: 0 when every case holds; 1 (naming the case, with the order it got)
#   when one does not.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI). python3, which the script
#   under test already requires.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

FIXTURE="$REPO_ROOT/scripts/test-data/publish-dry-run-order/metadata.json"
UNDER_TEST="$REPO_ROOT/scripts/publish-dry-run-order.sh"

KNOWN="optional_dep_order filter_closure dev_dep_edge"

ONLY=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --script)
      [ "$#" -ge 2 ] || { echo "publish-dry-run-order_selftest: --script needs a path." >&2; exit 2; }
      UNDER_TEST="$2"
      shift 2
      ;;
    -h|--help)
      grep '^#' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      case " $KNOWN " in
        *" $1 "*) ONLY="$ONLY $1" ;;
        *)
          echo "publish-dry-run-order_selftest: unknown case '$1'." >&2
          echo "  known: $KNOWN" >&2
          exit 2
          ;;
      esac
      shift
      ;;
  esac
done

[ -r "$FIXTURE" ] || {
  echo "publish-dry-run-order_selftest: fixture missing: $FIXTURE" >&2
  exit 2
}
[ -r "$UNDER_TEST" ] || {
  echo "publish-dry-run-order_selftest: script under test missing: $UNDER_TEST" >&2
  exit 2
}

PASSED=0
FAILED=0

# ---------------------------------------------------------------------------
# new_fixture_dir: a throwaway dir holding a copy of the script under test and
# a `cargo` shim. The script derives its repo root from its own BASH_SOURCE, so
# copying it in keeps the run hermetic. No `git init` needed — unlike the gates
# in check_scan_floor_selftest.sh, this script enumerates nothing from git.
# ---------------------------------------------------------------------------
new_fixture_dir() {
  local tmp
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/publishorder.XXXXXX")"
  mkdir -p "$tmp/scripts" "$tmp/bin"
  cp "$UNDER_TEST" "$tmp/scripts/publish-dry-run-order.sh"

  # The cargo shim. `cargo metadata` prints the fixture; WITHOUT --all-features
  # it first strips the optional demo-analyze -> demo-review edge, which is what
  # real cargo does when the gating feature is not activated. Any other cargo
  # subcommand is a bug in the test — the cases are --list-only, so the script
  # must never reach `cargo publish`.
  cat > "$tmp/bin/cargo" <<EOF
#!/usr/bin/env bash
if [ "\${1:-}" != "metadata" ]; then
  echo "cargo shim: refusing unexpected subcommand '\${1:-}' (selftest is --list-only)" >&2
  exit 97
fi
all_features=0
for a in "\$@"; do
  [ "\$a" = "--all-features" ] && all_features=1
done
exec python3 - "$FIXTURE" "\$all_features" <<'PYSHIM'
import json, sys
data = json.load(open(sys.argv[1]))
if sys.argv[2] != "1":
    # Model default-feature resolution: the optional, 'review'-gated edge is
    # simply not in the resolve graph.
    gated_from = "path+file:///w/crates/demo-analyze#demo-analyze@0.1.0"
    gated_to = "path+file:///w/crates/demo-review#demo-review@0.1.0"
    for node in data["resolve"]["nodes"]:
        if node["id"] == gated_from:
            node["dependencies"] = [d for d in node["dependencies"] if d != gated_to]
            node["deps"] = [d for d in node["deps"] if d["pkg"] != gated_to]
json.dump(data, sys.stdout)
PYSHIM
EOF
  chmod +x "$tmp/bin/cargo"
  echo "$tmp"
}

# ---------------------------------------------------------------------------
# order_of <fixture-dir> [crate ...] — run the script and print the computed
# order, one crate per line. The script writes the order to STDERR (stdout is
# left clean), indented two spaces under a header line.
# ---------------------------------------------------------------------------
order_of() {
  local dir="$1"
  shift
  ( cd "$dir" && PATH="$dir/bin:$PATH" \
      bash scripts/publish-dry-run-order.sh --list-only "$@" 2>&1 ) \
    | sed -n 's/^  \([a-z0-9-]*\)$/\1/p'
}

# ---------------------------------------------------------------------------
# expect_before <label> <order> <first> <second> — assert both crates are in
# the order and <first> precedes <second>.
# ---------------------------------------------------------------------------
expect_before() {
  local label="$1" order="$2" first="$3" second="$4"
  local i_first i_second
  # `|| true` on both: a MISSING crate is the single most informative failure
  # this file can report, and under `set -o pipefail` grep's exit 1 would
  # otherwise kill the run before the diagnostic below ever prints — losing
  # cases 2 and 3 behind case 1.
  i_first="$(printf '%s\n' "$order" | grep -n "^${first}\$" | head -1 | cut -d: -f1 || true)"
  i_second="$(printf '%s\n' "$order" | grep -n "^${second}\$" | head -1 | cut -d: -f1 || true)"

  if [ -z "$i_first" ]; then
    fail "$label" "'${first}' is missing from the order entirely" "$order"
    return
  fi
  if [ -z "$i_second" ]; then
    fail "$label" "'${second}' is missing from the order entirely" "$order"
    return
  fi
  if [ "$i_first" -ge "$i_second" ]; then
    fail "$label" "'${first}' must be published before '${second}', but came after" "$order"
    return
  fi

  echo "  ok  ${label} (${first} before ${second})"
  PASSED=$((PASSED + 1))
}

# ---------------------------------------------------------------------------
# expect_absent <label> <order> <crate> — assert a crate is NOT in the order.
# ---------------------------------------------------------------------------
expect_absent() {
  local label="$1" order="$2" crate="$3"
  if printf '%s\n' "$order" | grep -q "^${crate}\$"; then
    fail "$label" "'${crate}' must never appear in a publish order" "$order"
    return
  fi
  echo "  ok  ${label} (${crate} absent)"
  PASSED=$((PASSED + 1))
}

fail() {
  local label="$1" why="$2" order="$3"
  echo "SELF-TEST FAIL: ${label}" >&2
  echo "       ${why}" >&2
  echo "       computed order was:" >&2
  printf '%s\n' "$order" | sed 's/^/         /' >&2
  FAILED=$((FAILED + 1))
}

want() {
  [ -z "$ONLY" ] && return 0
  case " $ONLY " in
    *" $1 "*) return 0 ;;
  esac
  return 1
}

# ===========================================================================
# 1. The OPTIONAL, feature-gated edge (demo-analyze -> demo-review).
#    Without --all-features the shim withholds that edge, both crates come back
#    ready in the same Kahn round, and the alphabetical tie-break emits
#    demo-analyze first — a confident order that publishes a crate before the
#    sibling it needs. This is trusty-analyze vs trusty-review on 2026-08-10.
#
#    demo-private is asserted absent in the same run: it is `publish = false`
#    and depends on demo-analyze, so a regression that stopped honouring the
#    publishable filter would show up here rather than at a real publish.
# ===========================================================================
if want optional_dep_order; then
  d="$(new_fixture_dir)"
  o="$(order_of "$d")"
  expect_before "full order: optional edge is respected" "$o" demo-review demo-analyze
  expect_absent "full order: publish=false crate excluded" "$o" demo-private
  expect_absent "full order: external crate excluded" "$o" serde
  rm -rf "$d"
fi

# ===========================================================================
# 2. The SINGLE-CRATE FILTER — the form release.yml:1661 actually invokes
#    (`bash scripts/publish-dry-run-order.sh "<pkg_name>"`). The closure must
#    come back with the requested crate's publishable dependencies, in order.
#    Pre-fix this returned `demo-analyze` alone, because Kahn had emptied the
#    edge sets the closure walk reads — so the release job dry-ran one crate
#    and proved nothing about the siblings it needs.
# ===========================================================================
if want filter_closure; then
  d="$(new_fixture_dir)"
  o="$(order_of "$d" demo-analyze)"
  expect_before "scoped closure: sibling pulled in" "$o" demo-review demo-analyze
  expect_before "scoped closure: transitive dep pulled in" "$o" demo-common demo-review
  rm -rf "$d"
fi

# ===========================================================================
# 3. The DEV-ONLY edge (demo-mpm -> demo-review). `cargo publish` resolves a
#    full lockfile for the packaged crate, dev-dependencies included, so a
#    dev-only sibling must still be live first. The script gets this right by
#    reading resolve's flat `dependencies` list, which unions normal, build and
#    dev edges; this case exists so a later "tidy-up" that narrows to
#    `deps[].dep_kinds` normal edges fails here instead of at a publish.
#    trusty-mpm depends on trusty-review exactly this way.
# ===========================================================================
if want dev_dep_edge; then
  d="$(new_fixture_dir)"
  o="$(order_of "$d" demo-mpm)"
  expect_before "dev-only edge counts" "$o" demo-review demo-mpm
  rm -rf "$d"
fi

# ===========================================================================
# Verdict
# ===========================================================================
if [ "$FAILED" -ne 0 ]; then
  echo "publish-order self-test: ${PASSED} passed, ${FAILED} FAILED — see above (issue #5358)." >&2
  exit 1
fi

if [ "$PASSED" -eq 0 ]; then
  echo "publish-order self-test: 0 assertions ran — this self-test just proved nothing." >&2
  echo "  (check the case-name argument; a test that examines nothing is not a passing test.)" >&2
  exit 1
fi

echo "publish-order self-test: ${PASSED} assertion(s) hold over the fixture graph — OK."
exit 0
