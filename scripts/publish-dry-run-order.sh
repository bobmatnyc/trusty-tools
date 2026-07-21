#!/usr/bin/env bash
#
# publish-dry-run-order.sh — dependency-ordered `cargo publish --dry-run`
# preflight (issue #3366, requirement 2).
#
# Why: on 2026-07-20 a crate was published against sibling code that was not
#   yet live on crates.io — a publish-ORDERING mistake, distinct from (but
#   related to) the version-parity drift this issue's other guard
#   (scripts/check-version-parity.sh) detects. The established lesson from
#   that incident (see project memory "Cross-crate publish ordering") is:
#   `cargo publish --dry-run` resolves dependencies against the LIVE registry
#   and is the thing that actually catches "calls into a sibling crate that
#   isn't live yet" — but only if it is run for EVERY affected crate, IN
#   DEPENDENCY ORDER, immediately before a release. Today that ordering is a
#   manually-computed, documented recipe (.claude/skills/cargo-publish/SKILL.md
#   "Cross-Crate Publish Ordering") — this script makes the ordering itself
#   mechanical so a release doesn't depend on someone re-deriving the
#   dependency graph by hand under time pressure.
#
# What: runs `cargo metadata` once, computes a topological order over every
#   PUBLISHABLE workspace crate (i.e. not `publish = false`) using internal
#   (workspace-to-workspace) dependency edges only, then runs
#   `cargo publish --dry-run -p <crate>` for each crate in that order —
#   dependencies always before dependents. Any single dry-run failure stops
#   the run immediately (fail fast: a downstream crate's dry-run failing
#   because an upstream sibling isn't live yet is exactly the class of bug
#   this script exists to catch before a real publish).
#
# Usage:
#   scripts/publish-dry-run-order.sh              # every publishable crate, in order
#   scripts/publish-dry-run-order.sh --list-only  # print the order, run nothing
#   scripts/publish-dry-run-order.sh trusty-search trusty-common
#                                                  # only these crates (+ already
#                                                  # in the right relative order),
#                                                  # for a partial-release dry run
#
# Wired into CI (issue #3366 scope-extension): `.github/workflows/release.yml`
# runs this automatically, scoped to just the tagged crate (+ its publishable
# dependency closure), as its own `publish-dry-run` job on every `*-v*` tag
# push — see that job's header comment for why a single-crate-scoped,
# tag-triggered run is workable where a full-workspace-per-PR gate would not
# be (cost, registry rate limits), and why it is deliberately kept
# INDEPENDENT of (not blocking) the binary-release build/release jobs.
#
# Also usable manually (`make publish-dry-run-order`) exactly like
# check-publish-ready.sh and preflight-publish.sh, e.g. to dry-run a
# multi-crate release batch before tagging anything.
#
# Requires: python3 (present on GitHub Actions ubuntu-latest runners and on
#   every developer machine referenced elsewhere in this repo's scripts).
#
# Test: exercised manually — see the issue #3366 PR description for the raw
#   printed order against this workspace's real dependency graph, and for a
#   `--list-only` run confirming publishable-but-undeclared crates (e.g. a
#   brand-new crate) sort after everything they depend on.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

LIST_ONLY=0
FILTER=()
for arg in "$@"; do
  case "$arg" in
    --list-only) LIST_ONLY=1 ;;
    -h|--help)
      grep '^#' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) FILTER+=("$arg") ;;
  esac
done

# ---------------------------------------------------------------------------
# Compute the topological publish order via `cargo metadata` + a small Python
# helper (Kahn's algorithm, ties broken alphabetically for determinism).
#
# The metadata JSON is captured to a temp FILE (rather than piped directly
# into the python3 heredoc) because `python3 - <<'EOF' ... EOF` consumes
# stdin for the heredoc script body itself — piping `cargo metadata | python3
# -` would collide with that, silently starving the script of both its own
# source and the JSON. Passing the metadata as a file argument sidesteps the
# collision entirely.
# ---------------------------------------------------------------------------
TMP_METADATA="$(mktemp "${TMPDIR:-/tmp}/publish-dry-run-order.metadata.XXXXXX.json")"
trap 'rm -f "${TMP_METADATA}"' EXIT

cargo metadata --format-version=1 --locked >"${TMP_METADATA}"

ORDER="$(python3 - "${TMP_METADATA}" "${FILTER[@]}" <<'PYEOF'
import json
import sys

metadata_path = sys.argv[1]
filt = set(sys.argv[2:])
with open(metadata_path) as f:
    data = json.load(f)

members = set(data["workspace_members"])
packages = {p["id"]: p for p in data["packages"] if p["id"] in members}

# publish == None means "publishable to any registry"; [] means `publish = false`.
publishable = {pid for pid, p in packages.items() if p.get("publish") is None}

# Internal (workspace-to-workspace) dependency edges, keyed by package id.
resolve_nodes = {n["id"]: n for n in data["resolve"]["nodes"]}
edges = {pid: set() for pid in publishable}
for pid in publishable:
    node = resolve_nodes.get(pid)
    if not node:
        continue
    for dep_id in node.get("dependencies", []):
        if dep_id in publishable:
            edges[pid].add(dep_id)

# Kahn's algorithm: repeatedly emit any publishable crate whose remaining
# unemitted dependencies are all already emitted, breaking ties by name for
# deterministic output.
remaining = dict(edges)
emitted = []
name_of = {pid: packages[pid]["name"] for pid in publishable}
while remaining:
    ready = sorted(
        (pid for pid, deps in remaining.items() if not deps),
        key=lambda pid: name_of[pid],
    )
    if not ready:
        cyclic = ", ".join(sorted(name_of[pid] for pid in remaining))
        print(f"ERROR: dependency cycle among publishable crates: {cyclic}", file=sys.stderr)
        sys.exit(1)
    for pid in ready:
        emitted.append(pid)
        del remaining[pid]
    for deps in remaining.values():
        deps.difference_update(ready)

ordered_names = [name_of[pid] for pid in emitted]

if filt:
    # Keep only requested crates (still in dependency order) plus any
    # publishable crate they transitively depend on, so a partial run never
    # tries to dry-run a crate before an unlisted sibling it needs.
    pid_of = {v: k for k, v in name_of.items()}
    wanted = {pid_of[n] for n in filt if n in pid_of}
    closure = set()
    stack = list(wanted)
    while stack:
        pid = stack.pop()
        if pid in closure:
            continue
        closure.add(pid)
        stack.extend(edges.get(pid, ()))
    ordered_names = [n for n in ordered_names if pid_of[n] in closure]

for n in ordered_names:
    print(n)
PYEOF
)"

if [[ -z "${ORDER}" ]]; then
  echo "publish-dry-run-order: no publishable crates matched (check crate names)." >&2
  exit 1
fi

echo "publish-dry-run-order: dependency-ordered publish sequence:" >&2
echo "${ORDER}" | sed 's/^/  /' >&2

if [[ "${LIST_ONLY}" -eq 1 ]]; then
  exit 0
fi

# SKIP_UI_BUILD=1 unconditionally (matches ci.yml's global convention): the
# UI-embedding crates (trusty-search, trusty-memory, trusty-analyze,
# trusty-console) invoke pnpm from build.rs unless this is set, which fails
# inside `cargo publish`'s isolated verification tarball (no network/pnpm
# there) — see docs/reference/release-workflow.md's "UI-embedding crates"
# note. A no-op for every other crate, so it is simplest to always set it
# rather than special-case which crates in the computed order need it.
export SKIP_UI_BUILD=1

while IFS= read -r crate; do
  [[ -z "${crate}" ]] && continue
  echo "publish-dry-run-order: cargo publish --dry-run -p ${crate}" >&2
  if ! cargo publish --dry-run -p "${crate}"; then
    echo "publish-dry-run-order: FAILED at ${crate} — fix before publishing anything" \
         "after it in the order above (a downstream crate failing here usually" \
         "means an upstream sibling version isn't live on crates.io yet)." >&2
    exit 1
  fi
done <<<"${ORDER}"

echo "publish-dry-run-order: OK — every crate in the computed order passed --dry-run." >&2
