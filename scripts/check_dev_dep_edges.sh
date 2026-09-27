#!/usr/bin/env bash
#
# check_dev_dep_edges.sh — a crate's dev/build dependencies must not reach a
#   workspace crate its normal dependencies do not (issue #8341).
#
# Why: the point of separate crates is an efficient compilation process (owner
#   ruling, 2026-09-21). A `[dev-dependencies]` edge from crate A to crate B
#   welds their compile graphs together for EVERY `cargo test -p A` and every
#   `cargo clippy -p A --all-targets`, in every worktree — even though a
#   production `cargo build` never pays it, which is why the cost hides. #8341
#   measured five such edges: trusty-mpm -> trusty-review added 8 crates,
#   trusty-analyze -> {tga, trusty-audit, trusty-installer, trusty-console}
#   added ~135, trusty-code-gui -> trusty-code added 198, trusty-memory ->
#   trusty-installer added 13, and trusty-review -> {trusty-console,
#   trusty-installer} was dead weight no `use` site had referenced since #6354.
#   Workspace crates re-fingerprint per worktree path and sccache reports zero
#   hits for them, so nothing amortises the rebuild.
#
#   The fix for a genuine cross-crate test is a `publish = false` crate that
#   depends on BOTH sides normally — `crates/trusty-crate-contracts/`. This gate
#   is what stops the arrangement decaying back.
#
# What: reads `cargo metadata --format-version 1 --no-deps` — manifest-only, no
#   registry index, no network, no build — and for each workspace member C
#   computes the set of workspace members reachable from C through NORMAL
#   dependency edges. Every workspace member named in C's `dev-dependencies` or
#   `build-dependencies` that is NOT in that set is an offending edge, reported
#   as `C -> D`.
#
#   Only DIRECT dev/build edges are reported, and that loses nothing: if a
#   direct dev dependency D is already inside C's normal closure, so is
#   everything D reaches normally. The direct edge is also the only place the
#   fix can go.
#
#   An OPTIONAL normal dependency counts as normal. A dev edge that re-declares
#   a crate the normal tree already names — usually to turn a test-only feature
#   on, as trusty-common is re-declared in half the workspace — adds no crate to
#   the graph under resolver v2, so it is not what this gate is about.
#
#   Fails CLOSED. A metadata read that produces no packages, a package with no
#   name, and a malformed allowlist row are all failures; an unverifiable state
#   is never a silent pass. It enforces a SCAN FLOOR (#4618): examining fewer
#   than DEV_DEP_MIN_CRATES members exits non-zero rather than reporting success
#   over nothing.
#
# ALLOWLIST (ratchet): `.dev-dep-edge-allowlist.tsv`, one
#   `<consumer><TAB><dep><TAB><reason>` row per edge that must stay. The reason
#   must name an issue (`#1234`) so the row points at the work that removes it.
#   A row whose edge NO LONGER EXISTS is a failure — remove it. Prefer moving
#   the test over adding a row.
#
# Usage:
#   bash scripts/check_dev_dep_edges.sh          # check mode; exit 0 = clean
#   bash scripts/check_dev_dep_edges.sh --help
#
# Env (fixtures only):
#   DEV_DEP_METADATA    read this cargo-metadata JSON instead of running cargo
#   DEV_DEP_ALLOWLIST   use this allowlist instead of the repo's
#   DEV_DEP_MIN_CRATES  scan floor (default 20)
#
# Exit: 0 when every dev/build edge is covered; 1 on the first offending edge,
#   stale allowlist row, malformed row, or vacuous scan.
#
# Test: scripts/check_dev_dep_edges_selftest.sh drives this script over fixture
#   metadata — a clean workspace, a test-only edge, that edge allowlisted, a
#   stale row, a reason with no issue reference, a transitively-normal dep, a
#   build-dependency edge, and an empty package set (scan floor) — and asserts
#   the live repo scan passes.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI); POSIX tools, python3 for
#   the JSON read, and cargo only for the live metadata.

set -euo pipefail

case "${1:-}" in
  -h|--help)
    sed -n '2,70p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") ;;
  *)
    printf 'check_dev_dep_edges.sh: unknown option %s\n' "$1" >&2
    exit 1
    ;;
esac

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
ALLOWLIST="${DEV_DEP_ALLOWLIST:-${REPO_ROOT}/.dev-dep-edge-allowlist.tsv}"
MIN_CRATES="${DEV_DEP_MIN_CRATES:-20}"

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

META="${DEV_DEP_METADATA:-}"
if [ -z "${META}" ]; then
  if ! command -v cargo >/dev/null 2>&1; then
    printf '[FAIL] cargo is not on PATH, so the workspace graph cannot be read.\n' >&2
    printf '       This gate never guesses: install a toolchain or pass\n' >&2
    printf '       DEV_DEP_METADATA=<cargo-metadata.json>.\n' >&2
    exit 1
  fi
  META="${WORK}/metadata.json"
  # `--no-deps` is deliberate: the workspace-internal graph is entirely
  # declared in the member manifests, so nothing here needs the registry index
  # and the command stays offline and sub-second.
  if ! cargo metadata --format-version 1 --no-deps \
      --manifest-path "${REPO_ROOT}/Cargo.toml" > "${META}" 2>"${WORK}/cargo.err"; then
    printf '[FAIL] cargo metadata failed:\n' >&2
    cat "${WORK}/cargo.err" >&2
    exit 1
  fi
fi

if [ ! -f "${META}" ]; then
  printf '[FAIL] metadata file not found: %s\n' "${META}" >&2
  exit 1
fi

python3 - "${META}" "${ALLOWLIST}" "${MIN_CRATES}" <<'PY'
import json
import re
import sys

meta_path, allowlist_path, min_crates = sys.argv[1], sys.argv[2], int(sys.argv[3])

try:
    with open(meta_path, encoding="utf-8") as fh:
        meta = json.load(fh)
except (OSError, ValueError) as exc:
    print(f"[FAIL] could not read {meta_path}: {exc}", file=sys.stderr)
    sys.exit(1)

packages = meta.get("packages") or []
members = {}
for pkg in packages:
    name = pkg.get("name")
    if not name:
        print("[FAIL] a package in the metadata carries no name — "
              "the graph cannot be verified.", file=sys.stderr)
        sys.exit(1)
    members[name] = pkg

# Direct edges, split by kind, restricted to workspace members. cargo reports a
# normal dependency's kind as null; `--no-deps` output lists only members, so
# membership is a name lookup.
normal_direct = {}
testish_direct = {}
for name, pkg in members.items():
    normal, testish = set(), {}
    for dep in pkg.get("dependencies") or []:
        dep_name = dep.get("name")
        if dep_name not in members or dep_name == name:
            continue
        kind = dep.get("kind")
        if kind in (None, "", "normal"):
            normal.add(dep_name)
        elif kind in ("dev", "build"):
            testish.setdefault(dep_name, set()).add(kind)
    normal_direct[name] = normal
    testish_direct[name] = testish


def normal_closure(root):
    """Every workspace member reachable from `root` through normal edges."""
    seen, queue = set(), [root]
    while queue:
        cur = queue.pop()
        for nxt in normal_direct.get(cur, ()):
            if nxt not in seen:
                seen.add(nxt)
                queue.append(nxt)
    return seen


offending = {}
for name in sorted(members):
    reachable = normal_closure(name)
    for dep_name, kinds in sorted(testish_direct[name].items()):
        if dep_name not in reachable:
            offending[(name, dep_name)] = ", ".join(sorted(kinds))

# ---------------------------------------------------------------------------
# Allowlist: <consumer>\t<dep>\t<reason naming an issue>
# ---------------------------------------------------------------------------
ISSUE = re.compile(r"#\d+")
allowed, bad_rows = {}, []
try:
    with open(allowlist_path, encoding="utf-8") as fh:
        for lineno, raw in enumerate(fh, start=1):
            line = raw.rstrip("\n")
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            fields = line.split("\t")
            if len(fields) < 3:
                bad_rows.append(f"{allowlist_path}:{lineno}: expected three "
                                f"tab-separated fields, got {len(fields)}")
                continue
            consumer, dep, reason = fields[0].strip(), fields[1].strip(), fields[2].strip()
            if not consumer or not dep or not reason:
                bad_rows.append(f"{allowlist_path}:{lineno}: empty field")
                continue
            if not ISSUE.search(reason):
                bad_rows.append(f"{allowlist_path}:{lineno}: the reason must name an "
                                f"issue (#1234) so the row points at the work that "
                                f"removes it: {reason!r}")
                continue
            allowed[(consumer, dep)] = reason
except FileNotFoundError:
    allowed = {}
except OSError as exc:
    print(f"[FAIL] could not read {allowlist_path}: {exc}", file=sys.stderr)
    sys.exit(1)

failures = []
for msg in bad_rows:
    failures.append(f"malformed allowlist row — {msg}")

uncovered = [(c, d) for (c, d) in sorted(offending) if (c, d) not in allowed]
stale = [(c, d) for (c, d) in sorted(allowed) if (c, d) not in offending]

print(f"Checking dev/build dependency edges across {len(members)} workspace crate(s)")
for (consumer, dep) in sorted(offending):
    mark = "ALLOW" if (consumer, dep) in allowed else "FAIL "
    print(f"[{mark}] {consumer} -> {dep} ({offending[(consumer, dep)]}-dependency, "
          f"outside {consumer}'s normal tree)")

for (consumer, dep) in uncovered:
    kinds = offending[(consumer, dep)]
    failures.append(
        f"{consumer} -> {dep}: a {kinds}-dependency on a workspace crate that "
        f"{consumer}'s normal dependencies never reach.\n"
        f"         Every `cargo test -p {consumer}` and `cargo clippy -p {consumer} "
        f"--all-targets` now compiles {dep} and everything it pulls.\n"
        f"         Fix: move the test that needs both crates into "
        f"crates/trusty-crate-contracts/ (or another `publish = false` crate that\n"
        f"         depends on both NORMALLY). If the edge genuinely has to stay, add\n"
        f"         a row to .dev-dep-edge-allowlist.tsv naming the blocker and its issue:\n"
        f"             {consumer}\t{dep}\t<reason, naming #NNNN>"
    )

for (consumer, dep) in stale:
    failures.append(
        f"{consumer} -> {dep}: allowlisted, but that dev/build edge no longer "
        f"exists.\n         Fix: delete the row from .dev-dep-edge-allowlist.tsv — "
        f"the ratchet only shrinks."
    )

if len(members) < min_crates:
    print(f"[FAIL] SCAN FLOOR: examined {len(members)} workspace crate(s), expected "
          f"at least {min_crates}.", file=sys.stderr)
    print("       Either the metadata read returned almost nothing or the parser "
          "stopped", file=sys.stderr)
    print("       matching members. A vacuous scan is not a pass.", file=sys.stderr)
    sys.exit(1)

if failures:
    print("", file=sys.stderr)
    for failure in failures:
        print(f"[FAIL] {failure}", file=sys.stderr)
    print("", file=sys.stderr)
    print(f"{len(failures)} dev/build dependency edge problem(s) (#8341).", file=sys.stderr)
    sys.exit(1)

covered = len(allowed)
print("")
print(f"No workspace crate's dev/build dependencies reach outside its normal tree "
      f"({covered} allowlisted).")
PY
