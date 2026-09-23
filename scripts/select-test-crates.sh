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
#      `rust-toolchain.toml`, `.cargo/**`, `clippy.toml`, `rustfmt.toml`,
#      `deny.toml`                  -> ALL crates (every cargo invocation
#                                       reads these).
#   3. The affected-crate CI job's own inputs — `.github/workflows/ci.yml`,
#      this script, `scripts/ci-affected-test-plan.sh`, their selftests, and
#      the job's helpers `scripts/ci-create-local-main.sh`,
#      `scripts/ci-free-disk-space.sh`, `scripts/ci-apt-install.sh`
#                                    -> the CANARY set (trusty-common +
#                                       trusty-mpm), unioned with each Tauri
#                                       UI crate `ci-crate-relevance.sh`
#                                       answers `true` for over the same
#                                       change set. Direct only, no closure.
#   4. Any other `scripts/**` or `.github/**` path
#                                    -> each crate with a `*.rs` file (build.rs,
#                                       tests, include_str!, production code)
#                                       whose non-comment line names that path
#                                       literally, found by `git grep` at
#                                       selection time. Counts only when the
#                                       path EXISTS on disk or, in --range /
#                                       --staged mode, existed at the diff's
#                                       base (a deleted or renamed script), so
#                                       a fixture string like "scripts/go.sh"
#                                       selects nothing. `./`, `../`,
#                                       `{root}/` and `/abs/` prefixes all
#                                       name the path; `myscripts/` does not.
#                                       No reference -> NO crates.
#                                       Direct only: a build.rs failure shows
#                                       in the owning crate's own test run.
#      Plus: a `scripts/<name>.sh` directly in `scripts/` (no subdirectory,
#      extension exactly `sh`) whose content contains the substring
#      `codesign` — on disk, or at the diff's base for a deleted, renamed or
#      edited script -> trusty-common. This mirrors the directory scan
#      `codesign_scripts` in crates/trusty-common/src/launchd_labels/tests.rs,
#      read by `codesign_scripts_name_identifiers_by_convention`; change the
#      two together.
#   5. `docs/**`, `website/**`, a root-level `*.md`
#                                    -> NO crates.
#   6. anything else                -> ALL crates (fail open on an
#                                       unclassified path).
#   Rules 2-4: owner ruling 2026-09-23 on #7777. A literal scan that cannot
#   run (git grep error) fails open to ALL, and so does a canary crate or the
#   codesign crate that is not a workspace member.
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
#   scripts/select-test-crates.sh --files -- --odd.rs # `--`-prefixed path
#   scripts/select-test-crates.sh --range <a>..<b>    # explicit two-dot range
#   scripts/select-test-crates.sh --cargo-args         # `-p a -p b ...`
#
# Output: one crate `name` per line (default), or a single `-p a -p b ...`
#   line (--cargo-args). No output at all is a valid, correct answer for a
#   change set that maps to no crate (docs-only).
#
# Exit: 0 for every well-formed invocation, including every detection
#   failure covered by FAIL OPEN above (a bad `--range`, missing value on
#   `--range`, unresolvable ref, missing `cargo`/`jq`, bash <4 — see below).
#   A malformed CLI invocation — an argument this parser does not recognize
#   at all — exits 2 with a usage message on stderr; that scope exclusion is
#   deliberate (#7777 review) so a real usage typo stays visible to a human
#   or CI caller instead of silently vanishing into a full-workspace run. A
#   `--files` path that itself starts with `--` is NOT such a typo — pass it
#   after a literal `--` sentinel (see Usage above) and it is treated as a
#   path, exit 0, same as any other file.
#
# Requires bash 4+ (associative arrays). macOS ships bash 3.2.57 as
#   `/bin/bash`; under it this script detects the version up front, warns on
#   stderr, and routes through the same FAIL OPEN behavior — it never falls
#   through to bash 3.2's silent `declare -A` failure and an empty result
#   indistinguishable from "nothing to test" (#7777 review).
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
      # #7777: a literal `--` ends options so a `--`-prefixed path is still a path
      while [ $# -gt 0 ] && [ "$1" != "--" ] && [ "${1#--}" = "$1" ]; do
        FILES+=("$1")
        shift
      done
      if [ $# -gt 0 ] && [ "$1" = "--" ]; then
        shift
        while [ $# -gt 0 ]; do
          FILES+=("$1")
          shift
        done
      fi
      ;;
    --range)
      MODE="range"
      # #7777: consume one or two tokens so a bare --range cannot loop, and reject a flag-looking value
      if [ $# -ge 2 ] && [ "${2#--}" = "$2" ]; then
        RANGE_SPEC="$2"
        shift 2
      else
        RANGE_SPEC=""
        shift
      fi
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

# #7777: guard bash <4 before the first declare -A, which fails silently and empties the result
if [ "${BASH_VERSINFO[0]:-0}" -lt 4 ] 2>/dev/null; then
  echo "select-test-crates: WARNING: running under bash ${BASH_VERSION:-<unknown>} (need bash 4+ for associative arrays) — printing ALL crates" >&2
  BASH32_NAMES=""
  if command -v cargo >/dev/null 2>&1 && command -v jq >/dev/null 2>&1; then
    BASH32_NAMES="$(cargo metadata --no-deps --format-version 1 --offline 2>/dev/null | jq -r '.packages[].name' 2>/dev/null)"
    [ -n "$BASH32_NAMES" ] || BASH32_NAMES="$(cargo metadata --no-deps --format-version 1 2>/dev/null | jq -r '.packages[].name' 2>/dev/null)"
  fi
  if [ -z "$BASH32_NAMES" ]; then
    BASH32_ROOT="$(git rev-parse --show-toplevel 2>/dev/null)"
    if [ -n "$BASH32_ROOT" ] && [ -d "${BASH32_ROOT}/crates" ]; then
      for f in "${BASH32_ROOT}"/crates/*/Cargo.toml; do
        [ -f "$f" ] || continue
        n="$(awk '
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
        [ -n "$n" ] && BASH32_NAMES="${BASH32_NAMES}${BASH32_NAMES:+$'\n'}${n}"
      done
    fi
  fi
  if [ -z "$BASH32_NAMES" ]; then
    echo "select-test-crates: WARNING: could not determine any crate names under bash <4 — no output produced" >&2
    exit 0
  fi
  if [ "$CARGO_ARGS_MODE" = "1" ]; then
    BASH32_ARGS=""
    while IFS= read -r n; do
      [ -n "$n" ] || continue
      BASH32_ARGS="${BASH32_ARGS:+$BASH32_ARGS }-p $n"
      [ "$n" = "trusty-common" ] && BASH32_ARGS="${BASH32_ARGS} --features unconditional-only"
    done < <(printf '%s\n' "$BASH32_NAMES" | sort -u)
    [ -n "$BASH32_ARGS" ] && printf '%s\n' "$BASH32_ARGS"
  else
    printf '%s\n' "$BASH32_NAMES" | sort -u
  fi
  exit 0
fi

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

# #7777 ruling (b): what a change to the affected-crate job's own inputs tests.
CANARY_CRATES="trusty-common trusty-mpm"
# The crate whose test scans every codesign script (rule 4, codesign_script).
CODESIGN_CRATE="trusty-common"
# The crates ci.yml's detect-ui step asks ci-crate-relevance.sh about. Same
# list as that step's UI_CRATES and ci-affected-test-plan.sh's UI_CRATES.
RELEVANCE_CRATES="trusty-agents-ui trusty-audit-ui trusty-mpm-gui trusty-code-gui"
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

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
#
# #7777: try cargo metadata first; the shallow scan misses nested members (trusty-agents-ui)
fail_open() {
  local reason="$1"
  echo "select-test-crates: WARNING: ${reason} — printing ALL crates" >&2
  local -a crates=()
  if [ ${#ALL_CRATES[@]} -gt 0 ]; then
    crates=("${ALL_CRATES[@]}")
  else
    local meta=""
    if command -v cargo >/dev/null 2>&1 && command -v jq >/dev/null 2>&1; then
      meta="$(cargo metadata --no-deps --format-version 1 --offline 2>/dev/null)"
      [ -n "$meta" ] || meta="$(cargo metadata --no-deps --format-version 1 2>/dev/null)"
    fi
    if [ -n "$meta" ]; then
      while IFS= read -r line; do
        [ -n "$line" ] && crates+=("$line")
      done < <(printf '%s' "$meta" | jq -r '.packages[].name' 2>/dev/null)
    fi
    if [ ${#crates[@]} -eq 0 ]; then
      echo "select-test-crates: WARNING: cargo metadata unavailable — falling back to a shallow crates/*/Cargo.toml scan, which MISSES any nested workspace member (e.g. trusty-agents-ui, trusty-audit-ui) — this list may be incomplete" >&2
      while IFS= read -r line; do
        [ -n "$line" ] && crates+=("$line")
      done < <(fallback_all_crates)
    fi
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

# The diff's old side, for rule 4's existence check: a script deleted or
# renamed in the change set is absent on disk but still named by crates.
# `a...b` diffs from merge-base(a, b); `a..b` and a lone `a` diff from `a`.
DIFF_BASE=""
case "$MODE" in
  staged) DIFF_BASE="HEAD" ;;
  range)
    case "$RANGE_SPEC" in
      *...*)
        r_old="${RANGE_SPEC%%...*}"
        r_new="${RANGE_SPEC#*...}"
        DIFF_BASE="$(git merge-base "${r_old:-HEAD}" "${r_new:-HEAD}" 2>/dev/null)"
        ;;
      *..*)
        r_old="${RANGE_SPEC%%..*}"
        DIFF_BASE="${r_old:-HEAD}"
        ;;
      *) DIFF_BASE="$RANGE_SPEC" ;;
    esac
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

declare -A IS_MEMBER=()
while IFS=$'\t' read -r dir name; do
  [ -n "$dir" ] && [ -n "$name" ] || continue
  NAME_OF_DIR["$dir"]="$name"
  CRATE_DIRS+=("$dir")
  ALL_CRATES+=("$name")
  IS_MEMBER["$name"]=1
done <"${DIRMAP_FILE}"

# ---------------------------------------------------------------------------
# 4. Classify each changed path: an owning crate name, ALL, NONE, CANARY
#    (rule 3) or SCRIPTREF (rule 4).
# ---------------------------------------------------------------------------

# owning_crate <path> — the crate whose directory is the longest prefix of
# <path>, on a segment boundary; prints nothing when no crate owns it.
owning_crate() {
  local path="$1" d best="" bestlen=-1
  for d in "${CRATE_DIRS[@]}"; do
    if [ "$path" = "$d" ] || [ "${path#"$d"/}" != "$path" ]; then
      if [ ${#d} -gt "$bestlen" ]; then
        best="$d"
        bestlen=${#d}
      fi
    fi
  done
  [ -n "$best" ] && printf '%s\n' "${NAME_OF_DIR[$best]}"
}

classify_path() {
  local path="$1" owner
  owner="$(owning_crate "$path")"
  if [ -n "$owner" ]; then
    printf '%s\n' "$owner"
    return
  fi
  case "$path" in
    Cargo.toml | Cargo.lock | rust-toolchain | rust-toolchain.toml | clippy.toml | rustfmt.toml | deny.toml)
      echo "ALL"
      return
      ;;
    .cargo/*)
      echo "ALL"
      return
      ;;
    .github/workflows/ci.yml | scripts/select-test-crates.sh | scripts/select-test-crates_selftest.sh | scripts/ci-affected-test-plan.sh | scripts/ci-affected-test-plan-selftest.sh | \
      scripts/ci-create-local-main.sh | scripts/ci-free-disk-space.sh | scripts/ci-apt-install.sh)
      echo "CANARY"
      return
      ;;
    scripts/* | .github/*)
      echo "SCRIPTREF"
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

# crates_referencing <path> — rule 4. Prints the owning crate of every `*.rs`
# file (tracked or untracked, not ignored) with a non-comment line naming
# <path> literally. A <path> absent on disk, and absent at DIFF_BASE, references
# nothing. Returns 2 when git grep itself fails, so the caller can fail open.
crates_referencing() {
  local path="$1" re hits rc line file content
  if [ ! -f "${WORKSPACE_ROOT}/${path}" ]; then
    # #7777: a deleted or renamed script still counts if the diff's base had it
    [ -n "$DIFF_BASE" ] &&
      git -C "$WORKSPACE_ROOT" cat-file -e "${DIFF_BASE}:${path}" 2>/dev/null ||
      return 0
  fi
  re="$(printf '%s' "$path" | sed 's/[][\.*^$+?(){}|]/\\&/g')"
  # Path boundaries: a non-name byte before (`/` included, so `./`, `../`,
  # `{root}/` and `/abs/` prefixes match), no longer name after —
  # "scripts/go.sh" must not match "myscripts/go.sh" or "scripts/go.sh.bak".
  hits="$(git -C "$WORKSPACE_ROOT" grep --untracked -n -I -E \
    -e "(^|[^A-Za-z0-9_.-])${re}([^A-Za-z0-9_.-]|\.[^A-Za-z0-9]|\.?\$)" \
    -- '*.rs' </dev/null 2>/dev/null)"
  rc=$?
  [ "$rc" -le 1 ] || return 2
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    file="${line%%:*}"
    content="${line#*:}"
    content="${content#*:}"
    content="${content#"${content%%[![:space:]]*}"}"
    case "$content" in
      //* | /\** | \*/* | '* '* | '*') continue ;;
    esac
    owning_crate "$file"
  done <<<"$hits"
  return 0
}

# codesign_script <path> — true when <path> is one of the files trusty-common's
# `codesign_scripts` test helper scans: a `*.sh` directly in `scripts/` whose
# content contains "codesign", on disk or at DIFF_BASE. Keep in step with
# crates/trusty-common/src/launchd_labels/tests.rs `codesign_scripts`.
codesign_script() {
  local path="$1" name="${1#scripts/}" base_body
  [ "$name" != "$path" ] || return 1
  case "$name" in
    */* | .sh) return 1 ;; # a subdirectory, or a dotfile with no extension
    *.sh) : ;;
    *) return 1 ;;
  esac
  [ -f "${WORKSPACE_ROOT}/${path}" ] &&
    grep -qF codesign "${WORKSPACE_ROOT}/${path}" 2>/dev/null && return 0
  [ -n "$DIFF_BASE" ] || return 1
  # Captured, not piped into `grep -q`: an early grep exit SIGPIPEs cat-file
  # and pipefail would turn a match into a miss.
  base_body="$(git -C "$WORKSPACE_ROOT" cat-file blob "${DIFF_BASE}:${path}" 2>/dev/null)" || return 1
  [[ "$base_body" == *codesign* ]]
}

# relevant_ui_crates — rule 3's union: each RELEVANCE_CRATES member that
# ci-crate-relevance.sh answers `true` for over this change set. That script
# fails closed (`true`); a missing copy of it counts the same way.
relevant_ui_crates() {
  local relevance="${SELF_DIR}/ci-crate-relevance.sh" c verdict
  for c in $RELEVANCE_CRATES; do
    [ -n "${IS_MEMBER[$c]:-}" ] || continue
    verdict="true"
    if [ -f "$relevance" ]; then
      verdict="$(cd "$WORKSPACE_ROOT" && GITHUB_OUTPUT="" bash "$relevance" "$c" <"${CHANGED_FILE}" 2>/dev/null)"
    else
      echo "select-test-crates: WARNING: ${relevance} missing — counting ${c} relevant" >&2
    fi
    [ "$verdict" = "false" ] || printf '%s\n' "$c"
  done
}

declare -A DIRECT_SET=()
# Rules 3 and 4 select crates directly, never through the reverse closure.
declare -A EXTRA_SET=()
ANY_ALL=0
CANARY_HIT=0
CODESIGN_HIT=0
while IFS= read -r path; do
  [ -n "$path" ] || continue
  cls="$(classify_path "$path")"
  case "$cls" in
    ALL) ANY_ALL=1 ;;
    NONE) : ;;
    CANARY) CANARY_HIT=1 ;;
    SCRIPTREF)
      codesign_script "$path" && CODESIGN_HIT=1
      if ! refs="$(crates_referencing "$path")"; then
        echo "select-test-crates: WARNING: git grep failed scanning crates for '${path}' — printing ALL crates" >&2
        ANY_ALL=1
        continue
      fi
      while IFS= read -r c; do
        [ -n "$c" ] && EXTRA_SET["$c"]=1
      done <<<"$refs"
      ;;
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

if [ "$CANARY_HIT" = "1" ]; then
  for c in $CANARY_CRATES; do
    # #7777: a missing canary means the canary set no longer tests anything
    [ -n "${IS_MEMBER[$c]:-}" ] || fail_open "canary crate '${c}' is not a workspace member"
    EXTRA_SET["$c"]=1
  done
  while IFS= read -r c; do
    [ -n "$c" ] && EXTRA_SET["$c"]=1
  done < <(relevant_ui_crates)
fi

if [ "$CODESIGN_HIT" = "1" ]; then
  [ -n "${IS_MEMBER[$CODESIGN_CRATE]:-}" ] ||
    fail_open "codesign crate '${CODESIGN_CRATE}' is not a workspace member"
  EXTRA_SET["$CODESIGN_CRATE"]=1
fi

# Nothing owns a crate (docs, website, root md, an unreferenced script): print
# the direct picks, if any, and stop — there is no closure to walk.
if [ ${#DIRECT_SET[@]} -eq 0 ]; then
  [ ${#EXTRA_SET[@]} -eq 0 ] || emit_output "${!EXTRA_SET[@]}"
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

emit_output "${!CLOSURE[@]}" "${!EXTRA_SET[@]}"
