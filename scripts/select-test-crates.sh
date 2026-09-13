#!/usr/bin/env bash
#
# select-test-crates.sh — map a changed-file set to the workspace crates
#   whose tests that change can affect (#7753).
#
# Why: the test ladder in CLAUDE.md (rung 3: the changed crate; rung 4: add
#   each direct dependent) is applied by judgment today, and judgment
#   over-tests. One change that was mostly a prose markdown asset in
#   trusty-agents-common ran all 13,000 trusty-mpm tests plus trusty-code;
#   several agents have hit memory-pressure SIGKILLs running full suites in
#   parallel as a result. If a code path does not change, and nothing it
#   depends on changed either, its tests cannot have a new answer — crate-level
#   selection proves that mechanically instead of by eyeballing a diff.
#
# What: prints, one per line, the name (from each crate's own Cargo.toml, not
#   its directory) of every workspace crate whose test run the given change
#   set can affect: each crate that OWNS a changed file, plus the transitive
#   reverse-dependency closure of those crates (every crate that depends on
#   an owning crate, directly or through another workspace crate), where
#   "depends on" includes normal, dev and build dependencies — a
#   dev-dependency change can break a dependent's tests without touching its
#   library surface (see trusty-code-gui's dependency on trusty-code through a
#   compiled-in-constant test, documented in ci-crate-relevance.sh).
#
#   Workspace membership and each crate's on-disk directory come from
#   `cargo metadata --no-deps --format-version 1` (manifest-only, no registry
#   index, no network). The dependency edges used for the reverse closure come
#   from `cargo metadata --format-version 1` (the full resolve graph), filtered
#   to edges between two workspace members — an external registry crate never
#   appears in the closure or the output.
#
#   This intentionally does NOT reuse ci-crate-relevance.sh's inline Python:
#   that script answers a different question (is changed-set X inside crate
#   C's FORWARD closure, built from `--no-deps` manifest `dependencies`
#   arrays) than this one (which crates sit in the REVERSE closure of the
#   directly-changed set, built from the full resolve graph so dev/build
#   edges are captured the same way cargo itself resolves them). Reusing its
#   generic shape — build a dir/name map, then BFS over an edge set derived
#   from `cargo metadata`, fail open on any error — rather than its code: the
#   two scripts read different metadata and walk the graph in opposite
#   directions, so sharing the Python would mean threading a direction flag
#   through code whose only other caller is a required, already-selftested CI
#   gate. Duplicating the SHAPE, not the graph-source or the direction, is the
#   defensible reuse here; see the report for the alternative considered.
#
# File-to-crate mapping (in this order; first match wins):
#   1. `crates/<dir>/**`            -> the crate whose manifest lives at the
#                                       most specific (longest) matching
#                                       `crates/...` directory prefix.
#   2. Root `Cargo.toml`, `Cargo.lock`, `rust-toolchain`,
#      `rust-toolchain.toml`, `.cargo/**`, `clippy.toml`, `rustfmt.toml`
#                                    -> ALL crates (every cargo invocation
#                                       reads these).
#   3. `scripts/**`, `.github/**`   -> ALL crates (deliberately broad, not a
#                                       computed closure — see the report for
#                                       the narrower rule considered and
#                                       rejected in favor of this one).
#   4. `docs/**`, `website/**`, a root-level `*.md`
#                                    -> NO crates.
#   5. anything else                -> ALL crates (fail open on an
#                                       unclassified path).
#
# FAIL OPEN. A `cargo metadata` failure, a missing `jq`, or an empty/
#   unresolvable change set prints every crate cargo metadata (or, failing
#   that, a filesystem fallback scan of `crates/*/Cargo.toml`) can find, warns
#   on stderr, and exits 0 — same doctrine as ci-crate-relevance.sh: a broken
#   detector must cost a full test run, never a silent skip.
#
# trusty-common's `default` feature set is empty (#4901) — `-p trusty-common`
#   alone is a `compile_error!`. Which specific feature a change needs is not
#   mechanically derivable from a changed path: Cargo.toml declares feature
#   NAMES, not the source directories they gate. `--cargo-args` mode carries a
#   small, hand-maintained override table (CARGO_ARGS_FEATURE_OVERRIDES below)
#   with one entry, trusty-common -> `--features unconditional-only`: the
#   universally-compiling subset from CLAUDE.md's own feature table, correct
#   regardless of which gated module actually changed. A caller who knows the
#   change touched a specific gated module (memory_core, an embedder variant)
#   should widen manually per CLAUDE.md.
#
# Usage:
#   scripts/select-test-crates.sh                    # origin/main...HEAD
#   scripts/select-test-crates.sh --staged            # index + untracked
#   scripts/select-test-crates.sh --files a.rs b.rs   # explicit path list
#   scripts/select-test-crates.sh --range <a>..<b>    # explicit two-dot range
#   scripts/select-test-crates.sh --cargo-args         # `-p a -p b ...`
#
# Output: one crate `name` per line (default), or a single `-p a -p b ...`
#   line (--cargo-args). No output at all is a valid, correct answer for a
#   change set that maps to no crate (docs-only).
#
# Exit: always 0. See FAIL OPEN above.
#
# Test: scripts/select-test-crates_selftest.sh

set -uo pipefail

MODE="range"
RANGE_SPEC="origin/main...HEAD"
CARGO_ARGS_MODE=0
FILES=()

usage() {
  cat <<'EOF'
Usage: select-test-crates.sh [--staged | --files <path>... | --range <a>..<b>] [--cargo-args]
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --staged)
      MODE="staged"
      shift
      ;;
    --files)
      MODE="files"
      shift
      while [ $# -gt 0 ] && [ "${1#--}" = "$1" ]; do
        FILES+=("$1")
        shift
      done
      ;;
    --range)
      MODE="range"
      RANGE_SPEC="${2:-}"
      shift 2
      ;;
    --cargo-args)
      CARGO_ARGS_MODE=1
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "select-test-crates: unknown argument '$1'" >&2
      usage >&2
      exit 2
      ;;
  esac
done

TMPDIR_SELF="$(mktemp -d 2>/dev/null)" || {
  echo "select-test-crates: WARNING: cannot create a temp dir — no output produced" >&2
  exit 0
}
trap 'rm -rf "${TMPDIR_SELF}"' EXIT

CHANGED_FILE="${TMPDIR_SELF}/changed.txt"
MEMBERS_FILE="${TMPDIR_SELF}/members.json"
RESOLVE_FILE="${TMPDIR_SELF}/resolve.json"
DIRMAP_FILE="${TMPDIR_SELF}/dirmap.tsv"
EDGES_FILE="${TMPDIR_SELF}/edges.tsv"

declare -A NAME_OF_DIR=()
CRATE_DIRS=()
ALL_CRATES=()

# ---------------------------------------------------------------------------
# trusty-common's empty default feature set (#4901): a small, documented
# override rather than an attempted per-module mechanical guess. See header.
# ---------------------------------------------------------------------------
declare -A CARGO_ARGS_FEATURE_OVERRIDES=(
  [trusty-common]="--features unconditional-only"
)

# fallback_all_crates — last-resort crate-name scan needing neither cargo nor
# jq, for the case where `cargo metadata` itself is the thing that is broken.
# Reads `[package] name = "..."` out of every `crates/*/Cargo.toml` directly.
# Deliberately shallow (one path segment under `crates/`): it exists only to
# keep FAIL OPEN's promise alive when the mechanical path is unavailable, not
# to duplicate cargo's own workspace-member resolution.
fallback_all_crates() {
  local root f name
  root="$(git rev-parse --show-toplevel 2>/dev/null)" || return 1
  [ -d "${root}/crates" ] || return 1
  for f in "${root}"/crates/*/Cargo.toml; do
    [ -f "$f" ] || continue
    name="$(awk '
      /^\[package\]/ { inpkg = 1; next }
      /^\[/ { inpkg = 0 }
      inpkg && /^name[[:space:]]*=/ {
        line = $0
        sub(/^name[[:space:]]*=[[:space:]]*"/, "", line)
        sub(/".*/, "", line)
        print line
        exit
      }
    ' "$f")"
    [ -n "$name" ] && printf '%s\n' "$name"
  done
}

# emit_output <name>... — sort, dedupe, and print in the selected format.
emit_output() {
  local -a names=("$@")
  local -a sorted=()
  [ ${#names[@]} -eq 0 ] && return 0
  while IFS= read -r line; do
    [ -n "$line" ] && sorted+=("$line")
  done < <(printf '%s\n' "${names[@]}" | sort -u)
  [ ${#sorted[@]} -eq 0 ] && return 0
  if [ "$CARGO_ARGS_MODE" = "1" ]; then
    local -a args=()
    local n
    for n in "${sorted[@]}"; do
      args+=("-p" "$n")
      if [ -n "${CARGO_ARGS_FEATURE_OVERRIDES[$n]:-}" ]; then
        # shellcheck disable=SC2206 # override values are fixed, simple flag lists
        args+=(${CARGO_ARGS_FEATURE_OVERRIDES[$n]})
      fi
    done
    printf '%s\n' "${args[*]}"
  else
    printf '%s\n' "${sorted[@]}"
  fi
}

# fail_open <reason> — warn, print every crate we can still name, exit 0.
fail_open() {
  local reason="$1"
  echo "select-test-crates: WARNING: ${reason} — printing ALL crates" >&2
  local -a crates=()
  if [ ${#ALL_CRATES[@]} -gt 0 ]; then
    crates=("${ALL_CRATES[@]}")
  else
    while IFS= read -r line; do
      [ -n "$line" ] && crates+=("$line")
    done < <(fallback_all_crates)
  fi
  emit_output "${crates[@]}"
  exit 0
}

# ---------------------------------------------------------------------------
# 1. Resolve the change set.
# ---------------------------------------------------------------------------
case "$MODE" in
  files)
    [ ${#FILES[@]} -gt 0 ] || fail_open "no --files paths given"
    printf '%s\n' "${FILES[@]}" >"${CHANGED_FILE}"
    ;;
  staged)
    if ! {
      git diff -z --staged --name-only --no-renames 2>/dev/null
      git ls-files -z --others --exclude-standard 2>/dev/null
    } | tr '\0' '\n' >"${CHANGED_FILE}"; then
      fail_open "git could not read the staged/untracked change set"
    fi
    ;;
  range)
    [ -n "$RANGE_SPEC" ] || fail_open "no range given"
    if ! git diff -z --name-only --no-renames "$RANGE_SPEC" 2>/dev/null |
      tr '\0' '\n' >"${CHANGED_FILE}"; then
      fail_open "git diff over range '${RANGE_SPEC}' failed"
    fi
    ;;
esac

# A file of nothing but blank lines is the same "nothing to act on" case as a
# zero-byte file — strip blanks before judging emptiness.
sed -i.bak '/^[[:space:]]*$/d' "${CHANGED_FILE}" 2>/dev/null || true
rm -f "${CHANGED_FILE}.bak"
[ -s "${CHANGED_FILE}" ] || fail_open "empty change set"

# ---------------------------------------------------------------------------
# 2. Prerequisites.
# ---------------------------------------------------------------------------
command -v jq >/dev/null 2>&1 || fail_open "jq not on PATH"
command -v cargo >/dev/null 2>&1 || fail_open "cargo not on PATH"

if ! cargo metadata --no-deps --format-version 1 --offline >"${MEMBERS_FILE}" 2>/dev/null; then
  if ! cargo metadata --no-deps --format-version 1 >"${MEMBERS_FILE}" 2>/dev/null; then
    fail_open "cargo metadata --no-deps failed"
  fi
fi
[ -s "${MEMBERS_FILE}" ] || fail_open "cargo metadata --no-deps produced no output"

# ---------------------------------------------------------------------------
# 3. Workspace member dir/name map, from the manifest-only metadata.
# ---------------------------------------------------------------------------
WORKSPACE_ROOT="$(jq -r '.workspace_root // empty' "${MEMBERS_FILE}" 2>/dev/null)"
[ -n "$WORKSPACE_ROOT" ] || fail_open "cargo metadata --no-deps carried no workspace_root"

if ! jq -r --arg root "$WORKSPACE_ROOT" '
    .packages[]
    | (.manifest_path | rtrimstr("/Cargo.toml") | ltrimstr($root + "/")) as $dir
    | "\($dir)\t\(.name)"
  ' "${MEMBERS_FILE}" >"${DIRMAP_FILE}" 2>/dev/null; then
  fail_open "jq could not parse cargo metadata --no-deps output"
fi
[ -s "${DIRMAP_FILE}" ] || fail_open "workspace has no members"

while IFS=$'\t' read -r dir name; do
  [ -n "$dir" ] && [ -n "$name" ] || continue
  NAME_OF_DIR["$dir"]="$name"
  CRATE_DIRS+=("$dir")
  ALL_CRATES+=("$name")
done <"${DIRMAP_FILE}"

# ---------------------------------------------------------------------------
# 4. Classify each changed path: an owning crate name, ALL, or NONE.
# ---------------------------------------------------------------------------
classify_path() {
  local path="$1" d best="" bestlen=-1
  for d in "${CRATE_DIRS[@]}"; do
    if [ "$path" = "$d" ] || [ "${path#"$d"/}" != "$path" ]; then
      if [ ${#d} -gt "$bestlen" ]; then
        best="$d"
        bestlen=${#d}
      fi
    fi
  done
  if [ -n "$best" ]; then
    printf '%s\n' "${NAME_OF_DIR[$best]}"
    return
  fi
  case "$path" in
    Cargo.toml | Cargo.lock | rust-toolchain | rust-toolchain.toml | clippy.toml | rustfmt.toml)
      echo "ALL"
      return
      ;;
    .cargo/*)
      echo "ALL"
      return
      ;;
    scripts/* | .github/*)
      echo "ALL"
      return
      ;;
    docs/* | website/*)
      echo "NONE"
      return
      ;;
  esac
  case "$path" in
    */*) : ;; # has a directory component, not a root-level file
    *.md)
      echo "NONE"
      return
      ;;
  esac
  echo "ALL"
}

declare -A DIRECT_SET=()
ANY_ALL=0
while IFS= read -r path; do
  [ -n "$path" ] || continue
  cls="$(classify_path "$path")"
  case "$cls" in
    ALL) ANY_ALL=1 ;;
    NONE) : ;;
    *) DIRECT_SET["$cls"]=1 ;;
  esac
done <"${CHANGED_FILE}"

# ---------------------------------------------------------------------------
# 5. Workspace-wide input touched: skip the closure, every crate is affected.
# ---------------------------------------------------------------------------
if [ "$ANY_ALL" = "1" ]; then
  emit_output "${ALL_CRATES[@]}"
  exit 0
fi

# Docs/website/root-md-only change: nothing owns a crate, nothing to test.
if [ ${#DIRECT_SET[@]} -eq 0 ]; then
  exit 0
fi

# ---------------------------------------------------------------------------
# 6. Reverse-dependency closure, from the full resolve graph.
# ---------------------------------------------------------------------------
if ! cargo metadata --format-version 1 --offline >"${RESOLVE_FILE}" 2>/dev/null; then
  if ! cargo metadata --format-version 1 >"${RESOLVE_FILE}" 2>/dev/null; then
    fail_open "cargo metadata (resolve graph) failed"
  fi
fi
[ -s "${RESOLVE_FILE}" ] || fail_open "cargo metadata (resolve graph) produced no output"

# Reverse edges only, workspace members only, every dependency kind (normal,
# dev, build) included — a dev-dependency change can break a dependent's
# tests. Output: "<dependency-name>\t<dependent-name>" per edge.
if ! jq -r '
    (.workspace_members | map({(.): true}) | add // {}) as $wsids
    | (reduce (.packages[] | select($wsids[.id])) as $p ({}; . + {($p.id): $p.name})) as $id2name
    | (.resolve.nodes[] | select($wsids[.id])) as $node
    | ($node.deps // [])[] | select($wsids[.pkg])
    | "\($id2name[.pkg])\t\($id2name[$node.id])"
  ' "${RESOLVE_FILE}" >"${EDGES_FILE}" 2>/dev/null; then
  fail_open "jq could not parse cargo metadata (resolve graph) output"
fi

declare -A REV_ADJ=()
while IFS=$'\t' read -r dep dependent; do
  [ -n "$dep" ] && [ -n "$dependent" ] || continue
  REV_ADJ["$dep"]="${REV_ADJ[$dep]:-}${REV_ADJ[$dep]:+ }${dependent}"
done <"${EDGES_FILE}"

declare -A CLOSURE=()
QUEUE=()
for n in "${!DIRECT_SET[@]}"; do
  CLOSURE["$n"]=1
  QUEUE+=("$n")
done
while [ ${#QUEUE[@]} -gt 0 ]; do
  cur="${QUEUE[0]}"
  QUEUE=("${QUEUE[@]:1}")
  for nxt in ${REV_ADJ[$cur]:-}; do
    if [ -z "${CLOSURE[$nxt]:-}" ]; then
      CLOSURE["$nxt"]=1
      QUEUE+=("$nxt")
    fi
  done
done

emit_output "${!CLOSURE[@]}"
