#!/usr/bin/env bash
#
# ci-crate-relevance.sh — decide whether a change set can affect one crate's
#   build (#7063).
#
# Why: the four Tauri UI clippy jobs in .github/workflows/ci.yml are required
#   contexts, so each must run and report a check run on every PR — a job that
#   never reports leaves the PR BLOCKED rather than fast (#4468). Their only
#   step gate was `docs_only`, so a PR touching an unrelated crate still paid
#   for the WebKit2GTK apt chain plus a cargo clippy. On PR #7062
#   `trusty-mpm-gui`, which has no workspace dependencies at all, spent 1m20s;
#   `trusty-code-gui` spent 7m and has hit its 30-minute timeout before
#   (PR #5992).
#
# What: prints `true` when any changed path is under the named crate's own
#   directory, under the directory of any workspace crate in its transitive
#   dependency closure, or is a workspace-wide input every cargo invocation in
#   those jobs reads; prints `false` otherwise.
#
#   The closure is derived at run time from `cargo metadata --format-version 1
#   --no-deps`, never a hand-maintained list, so adding or dropping a
#   dependency edge changes the answer on the PR that does it. `--no-deps`
#   reads only the workspace member manifests: it needs no registry index and
#   no network, and it still reports every path dependency, which is the only
#   kind that can name another crate in this repo.
#
#   Dev- and build-dependencies are in the closure, not just normal ones:
#   `cargo clippy --all-targets` and `cargo test` both compile the test
#   targets. trusty-code-gui reaches trusty-code exactly that way, through the
#   `default_daemon_url_matches_tcode_default_http_port` test that pins its
#   compiled-in DEFAULT_DAEMON_URL against trusty-code's DEFAULT_HTTP_PORT.
#
#   Matching is PATH-based and on SEGMENT boundaries, never on crate names and
#   never on string prefixes: `crates/trusty-mpm` must not match
#   `crates/trusty-mpm-gui`, and a crate whose directory nests inside another's
#   (trusty-audit-ui lives at crates/trusty-audit/ui/src-tauri) is matched by
#   its own directory as well as its parent's.
#
#   FAIL CLOSED. Every error — no crate name, a crate cargo does not know, a
#   `cargo metadata` failure, a missing python3, an unresolvable base ref, an
#   empty change set — prints `true`, writes a note to stderr, and exits 0, so
#   the job does the work rather than skipping it. A broken detector costs a
#   full build; it can never cost a silent skip.
#
# Usage:
#   git diff --name-only --no-renames "$MERGE_BASE" HEAD |
#     bash scripts/ci-crate-relevance.sh trusty-mpm-gui
#   CRATE_RELEVANCE_BASE=origin/main bash scripts/ci-crate-relevance.sh trusty-code-gui
#
# Output: `true` or `false` alone on stdout. When $GITHUB_OUTPUT is set, also
#   appends two lines, the crate name with `-` replaced by `_`:
#     <crate>_relevant=true|false
#     <crate>_reason=<one line naming the path that decided it, or the closure>
#
# Exit: always 0. See FAIL CLOSED above.
#
# Test: scripts/ci-crate-relevance-selftest.sh

# No `set -e`: every failure path here has to reach the fail-closed emit
# rather than abort the script with no verdict written.
set -uo pipefail

CRATE="${1:-}"

TMPDIR_SELF=""

# emit <true|false> <one-line reason> — write the verdict everywhere and stop.
emit() {
  local verdict="$1" reason="$2" key
  key="$(printf '%s' "${CRATE:-unknown}" | tr '-' '_')"
  echo "ci-crate-relevance: ${CRATE:-<no crate>} -> ${verdict} (${reason})" >&2
  echo "${verdict}"
  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    printf '%s_relevant=%s\n' "$key" "$verdict" >>"${GITHUB_OUTPUT}"
    printf '%s_reason=%s\n' "$key" "$reason" >>"${GITHUB_OUTPUT}"
  fi
  exit 0
}

fail_closed() { emit true "fail closed: $1"; }

[ -n "$CRATE" ] || fail_closed "no crate name given"

command -v python3 >/dev/null 2>&1 || fail_closed "python3 not on PATH"

TMPDIR_SELF="$(mktemp -d 2>/dev/null)" || fail_closed "cannot create a temp dir"
# Armed only once the directory exists, so the handler never has an empty path
# to remove. `emit` exits, so this covers the fail-closed arms too.
trap 'rm -rf "${TMPDIR_SELF}"' EXIT
CHANGED_FILE="${TMPDIR_SELF}/changed.txt"
METADATA_FILE="${TMPDIR_SELF}/metadata.json"

# Resolve the change set: a base ref we diff ourselves, or a list on stdin.
if [ -n "${CRATE_RELEVANCE_BASE:-}" ]; then
  if ! merge_base="$(git merge-base "${CRATE_RELEVANCE_BASE}" HEAD 2>/dev/null)"; then
    fail_closed "cannot resolve merge-base against '${CRATE_RELEVANCE_BASE}'"
  fi
  if ! git diff --name-only --no-renames "${merge_base}" HEAD >"${CHANGED_FILE}" 2>/dev/null; then
    fail_closed "git diff against ${merge_base} failed"
  fi
else
  cat >"${CHANGED_FILE}"
fi

[ -s "${CHANGED_FILE}" ] || fail_closed "empty change set"

# --offline first so a warm checkout never touches the network; the plain
# retry covers a checkout where cargo still wants to write its own caches.
if ! cargo metadata --format-version 1 --no-deps --offline >"${METADATA_FILE}" 2>/dev/null; then
  if ! cargo metadata --format-version 1 --no-deps >"${METADATA_FILE}" 2>/dev/null; then
    fail_closed "cargo metadata failed"
  fi
fi
[ -s "${METADATA_FILE}" ] || fail_closed "cargo metadata produced no output"

# The closure walk and the path matching. Prints the verdict on line 1 and the
# one-line reason on line 2; every per-path decision goes to stderr so the CI
# log records exactly which files and which closure produced the answer.
if ! decision="$(
  CRATE_NAME="$CRATE" \
    CHANGED_FILE="$CHANGED_FILE" \
    METADATA_FILE="$METADATA_FILE" \
    python3 - <<'PY'
import json
import os
import sys

crate = os.environ["CRATE_NAME"]

with open(os.environ["METADATA_FILE"], encoding="utf-8") as fh:
    meta = json.load(fh)
with open(os.environ["CHANGED_FILE"], encoding="utf-8") as fh:
    changed = [line.strip() for line in fh if line.strip()]

root = meta.get("workspace_root")
packages = meta.get("packages") or []
if not root or not packages:
    raise SystemExit("cargo metadata carried no workspace_root or packages")

# The bash side rejects an EMPTY file; a file of nothing but blank lines
# survives that and arrives here as an empty list. Answering `false` for it
# would let an unreadable diff turn into a skip, which is the one outcome this
# script exists to prevent.
if not changed:
    print("true")
    print("fail closed: change set has no usable paths")
    sys.exit(0)


def reldir(manifest_path):
    """Directory of a manifest, relative to the workspace root, with / separators."""
    d = os.path.relpath(os.path.dirname(manifest_path), root)
    return d.replace(os.sep, "/")


dir_of_name = {}
name_of_dir = {}
for pkg in packages:
    d = reldir(pkg["manifest_path"])
    dir_of_name[pkg["name"]] = d
    name_of_dir[d] = pkg["name"]

if crate not in dir_of_name:
    print("true")
    print("fail closed: '%s' is not a workspace member cargo metadata knows" % crate)
    sys.exit(0)

# Workspace-internal edges, from path dependencies of every kind (normal, dev
# and build). A path dependency that resolves outside the workspace members
# still contributes its directory, so an in-repo non-member crate is not lost.
edges = {}
extra_dirs = set()
for pkg in packages:
    deps = set()
    for dep in pkg.get("dependencies") or []:
        dep_path = dep.get("path")
        if not dep_path:
            continue
        d = os.path.relpath(dep_path, root).replace(os.sep, "/")
        if d in name_of_dir:
            deps.add(name_of_dir[d])
        elif not d.startswith(".."):
            extra_dirs.add(d)
    edges[pkg["name"]] = deps

closure = {crate}
stack = [crate]
while stack:
    current = stack.pop()
    for nxt in edges.get(current, ()):
        if nxt not in closure:
            closure.add(nxt)
            stack.append(nxt)

closure_dirs = {dir_of_name[n] for n in closure} | extra_dirs
closure_names = sorted(closure)

# Inputs no crate directory covers but every cargo invocation in these jobs
# reads. `.cargo/**` is a prefix rule; the rest are exact paths.
WORKSPACE_WIDE_EXACT = {
    "Cargo.toml",
    "Cargo.lock",
    "clippy.toml",
    "rust-toolchain",
    "rust-toolchain.toml",
    ".github/workflows/ci.yml",
    "scripts/ci-apt-install.sh",
    "scripts/ci-crate-relevance.sh",
    "scripts/ci-crate-relevance-selftest.sh",
}
WORKSPACE_WIDE_PREFIXES = (".cargo/",)


def under(path, directory):
    """True when `path` is `directory` or sits inside it, on segment boundaries."""
    return path == directory or path.startswith(directory + "/")


def workspace_wide(path):
    if path in WORKSPACE_WIDE_EXACT:
        return True
    return any(path.startswith(prefix) for prefix in WORKSPACE_WIDE_PREFIXES)


shown = ", ".join(closure_names[:8])
if len(closure_names) > 8:
    shown += ", +%d more" % (len(closure_names) - 8)
print(
    "ci-crate-relevance: %s closure = %s" % (crate, shown),
    file=sys.stderr,
)

reason = None
for path in changed:
    if workspace_wide(path):
        print("  relevant: %s (workspace-wide cargo input)" % path, file=sys.stderr)
        if reason is None:
            reason = "%s is a workspace-wide cargo input" % path
        continue
    hit = None
    for directory in closure_dirs:
        if under(path, directory):
            # Prefer the most specific directory when crate dirs nest.
            if hit is None or len(directory) > len(hit):
                hit = directory
    if hit is not None:
        owner = name_of_dir.get(hit, hit)
        print("  relevant: %s (under %s, %s)" % (path, hit, owner), file=sys.stderr)
        if reason is None:
            reason = "%s is under %s (%s), in the %s closure" % (path, hit, owner, crate)
    else:
        print("  inert   : %s" % path, file=sys.stderr)

if reason is None:
    print("false")
    print(
        "none of %d changed path(s) touch the %s closure (%s) or a workspace-wide input"
        % (len(changed), crate, shown)
    )
else:
    print("true")
    print(reason)
PY
)"; then
  fail_closed "closure computation failed"
fi

verdict="$(printf '%s\n' "$decision" | sed -n '1p')"
reason="$(printf '%s\n' "$decision" | sed -n '2p')"

case "$verdict" in
  true | false) ;;
  *) fail_closed "closure computation printed an unusable verdict '${verdict}'" ;;
esac
[ -n "$reason" ] || reason="no reason recorded"

emit "$verdict" "$reason"
